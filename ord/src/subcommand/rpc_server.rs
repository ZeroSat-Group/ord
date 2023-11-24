mod errors;
#[macro_use]
mod macros;

use super::*;
use crate::envelope::RawEnvelope;
use crate::index::{BlockHashWithTxid, Index, INDEX_CACHE};
use crate::options::Options;
use crate::subcommand::rpc_server::errors::{OptionExt, RpcServerError, RpcServerResult};
use crate::templates::{InscriptionOnTx, InscriptionRaw, InscriptionRpcContent, TransactionRpc};
use base64::Engine;
use bitcoin::absolute;
use bitcoin::psbt::Psbt;
use bitcoincore_rpc::json::GetRawTransactionResult;
use clap::Parser;
use indexmap::IndexMap;
use jsonrpsee_core::{JsonValue, TEN_MB_SIZE_BYTES};
use jsonrpsee_server::types::ErrorObjectOwned;
use jsonrpsee_server::{RpcModule, ServerBuilder};
use parking_lot::{Mutex, RwLock};
use redb::ReadTransaction;
use signal_hook::consts::{SIGABRT, SIGINT, SIGQUIT, SIGTERM};
use signal_hook_tokio::Signals;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio_stream::StreamExt;
use uuid::Uuid;

lazy_static! {
  pub static ref LOG_DETAILS: AtomicBool = AtomicBool::new(false);
}

struct ModuleContext {
  index: Arc<Index>,
  chain: Chain,
  pool: Arc<rayon::ThreadPool>,
}

#[derive(Debug, Parser)]
pub(crate) struct RpcServer {
  #[arg(
    long,
    default_value = "0.0.0.0",
    help = "Listen on <ADDRESS> for incoming requests."
  )]
  address: String,
  #[arg(
    long,
    help = "Request ACME TLS certificate for <ACME_DOMAIN>. This ord instance must be reachable at <ACME_DOMAIN>:443 to respond to Let's Encrypt ACME challenges."
  )]
  #[arg(
    long,
    help = "Listen on <RPC_PORT> for incoming RPC requests. [default: 8000]."
  )]
  rpc_port: Option<u16>,
  acme_domain: Vec<String>,
  #[arg(long, help = "Store ACME TLS certificates in <ACME_CACHE>.")]
  acme_cache: Option<PathBuf>,
  #[arg(long, help = "Provide ACME contact <ACME_CONTACT>.")]
  acme_contact: Vec<String>,
}

impl RpcServer {
  pub(crate) fn run(self, options: Options, index: Arc<Index>) -> SubcommandResult {
    Runtime::new()?.block_on(async {
      let rayon_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
          .num_threads(num_cpus::get())
          .build()
          .unwrap(),
      );

      tokio::spawn(async move {
        match Signals::new([SIGABRT, SIGTERM, SIGINT, SIGQUIT]) {
          Ok(mut signals) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                Some(sig) = signals.next() => {
                    match sig {
                        SIGABRT | SIGTERM | SIGINT | SIGQUIT => {}
                        _ => unreachable!(),
                    }
                }
            }

            log::debug!("Sig received, stopping Rpc Server");
            let rpc_listeners = RPC_LISTENERS.lock().await;
            if !rpc_listeners.is_empty() {
              log::info!("RPC_LISTENERS shutting down");
              for h in rpc_listeners.iter() {
                h.stop().unwrap();
              }
            }
          }
          Err(err) => {
            log::error!("Unable to register signal handlers: {:?}", err);
            process::exit(1);
          }
        }
      });

      let clone = index.clone();
      thread::spawn(move || {
        UPDATER_ON.store(true, atomic::Ordering::Relaxed);
        loop {
          if SHUTTING_DOWN.load(atomic::Ordering::Relaxed) {
            log::info!("Updater shutting down");
            break;
          }

          if let Err(error) = clone.update() {
            log::warn!("{error}");
          }
          thread::sleep(Duration::from_millis(5000));
        }
        UPDATER_ON.store(false, atomic::Ordering::Relaxed);
      });

      let module = self.rpc_module(index.clone(), options.chain(), rayon_pool);

      Self::start_json_rpc(module, self.port())?.await??;

      drop(index);
      Ok(Box::new(Empty {}) as Box<dyn Output>)
    })
  }

  fn port(&self) -> u16 {
    if let Some(p) = self.rpc_port {
      p
    } else {
      8000
    }
  }

  fn start_json_rpc(
    module: RpcModule<ModuleContext>,
    port: u16,
  ) -> Result<task::JoinHandle<Result<()>>> {
    Ok(tokio::spawn(async move {
      let server = ServerBuilder::default()
        .max_response_body_size(5 << 30)
        .max_request_body_size(TEN_MB_SIZE_BYTES)
        .build(format!("0.0.0.0:{port}"))
        .await?;
      let s_h = server.start(module);
      RPC_ON.store(true, atomic::Ordering::Relaxed);
      RPC_LISTENERS.lock().await.push(s_h.clone());
      eprintln!("Rpc Listening on: http://0.0.0.0:{}", port);
      s_h.stopped().await;
      log::info!("Rpc Server stopped");
      SHUTTING_DOWN.store(true, atomic::Ordering::Relaxed);
      wait_updater_off();
      RPC_ON.store(false, atomic::Ordering::Relaxed);
      Ok(())
    }))
  }

  fn rpc_module(
    &self,
    index: Arc<Index>,
    chain: Chain,
    rayon_pool: Arc<rayon::ThreadPool>,
  ) -> RpcModule<ModuleContext> {
    let mut module = RpcModule::new(ModuleContext {
      index,
      chain,
      pool: rayon_pool,
    });

    module
      .register_method("psbt_extract_tx", |params, _| {
        let psbt_words = params.one::<String>()?;
        let psbt = Psbt::from_str(&psbt_words)
          .map_err(Error::from)
          .map_err(RpcServerError::from)?;
        let tx = psbt.extract_tx();
        Ok::<Transaction, ErrorObjectOwned>(tx)
      })
      .unwrap();

    module
      .register_method("log_details", |params, _| {
        let switch: bool = params.one()?;
        LOG_DETAILS.store(switch, Ordering::Relaxed);
        log::info!("log_details set to {}", switch);
        Ok::<(), ErrorObjectOwned>(())
      })
      .unwrap();

    module
      .register_async_method("block_time", |params, data| async move {
        let height = params.one::<u64>()?;
        Self::block_time(data.index.clone(), height)
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    module
      .register_async_method("transactions_filter", |params, data| async move {
        let (height, addrs): (u64, Vec<String>) = params.parse()?;

        let mut sellers = vec![];
        for addr in addrs {
          sellers.push(
            Address::from_str(&addr)
              .map_err(Error::from)
              .map_err(RpcServerError::from)?
              .require_network(data.chain.network())
              .map_err(Error::from)
              .map_err(RpcServerError::from)?,
          )
        }

        Self::transactions_filter(data.index.clone(), data.chain, height, sellers)
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    module
      .register_async_method("inscriptions_count", |_, data| async move {
        Self::inscriptions_count(data.index.clone())
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    // block height []
    module
      .register_async_method("blockheight", |_, data| async move {
        Self::block_height(data.index.clone())
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    // inscription_content [inscription_id]
    module
      .register_async_method("inscription", |params, data| async move {
        let inscription_id: InscriptionId = params.one()?;
        Self::inscription(data.index.clone(), data.chain, inscription_id)
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    // inscription_content [inscription_id]
    module
      .register_async_method("inscription_raw", |params, data| async move {
        let inscription_id: InscriptionId = params.one()?;
        Self::inscription_data(data.index.clone(), inscription_id)
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    module
      .register_async_method("inscription_content", |params, data| async move {
        let inscription_id: InscriptionId = params.one()?;
        Self::inscription_content(data.index.clone(), inscription_id)
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    module
      .register_async_method("txids_by_height", |params, data| async move {
        let (height, only_show_ins): (u64, bool) = params.parse()?;
        let latest = data
          .index
          .block_height()
          .map_err(|e| ErrorObjectOwned::from(RpcServerError::Internal(e)))?
          .ok_or_not_found(|| "newest height")?
          .0;
        if height > latest {
          return Err(ErrorObjectOwned::from(RpcServerError::BadRequest(format!(
            "given height {height} exceeds the latest one {latest}"
          ))));
        }
        Self::txids_by_height(
          data.index.clone(),
          Height(height),
          data.pool.clone(),
          only_show_ins,
        )
        .await
        .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    // transaction [txid]
    module
      .register_async_method("transaction", |params, data| async move {
        let mut ps = params.sequence();
        let mut txids = Vec::new();
        let mut show_tx_detail = false;
        while let Ok(value) = ps.next::<JsonValue>() {
          if let Ok(txid) = serde_json::from_value::<Txid>(value.clone()) {
            txids.push(txid);
            continue;
          }
          if let Ok(show) = serde_json::from_value::<bool>(value.clone()) {
            show_tx_detail = show;
            continue;
          }
          return Err(ErrorObjectOwned::from(RpcServerError::BadRequest(format!(
            "invalid params {}",
            value
          ))));
        }
        Self::transaction(
          data.chain,
          data.index.clone(),
          data.pool.clone(),
          txids,
          show_tx_detail,
        )
        .await
        .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    module
      .register_async_method("inscriptions_on_output", |params, data| async move {
        let outpoint: OutPoint = params.one()?;
        Self::inscriptions_on_output_record(data.index.clone(), outpoint)
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    module
      .register_async_method("tx_index", |params, data| async move {
        let (blockhash, txid): (BlockHash, Txid) = params.parse()?;
        Self::get_tx_index(data.index.clone(), blockhash, txid)
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    module
      .register_async_method("outpoint_range", |params, data| async move {
        let outpoint: OutPoint = params.one()?;
        Self::outpoint_range(data.index.clone(), outpoint)
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    module
      .register_async_method("getrawtransaction", |params, data| async move {
        let txid: Txid = params.one()?;
        Self::getrawtransaction(txid, data.index.clone())
          .await
          .map_err(ErrorObjectOwned::from)
      })
      .unwrap();

    module
  }

  async fn inscriptions_count(index: Arc<Index>) -> RpcServerResult<u64> {
    Ok(index.inscriptions_count()?)
  }

  fn pre_outputs_belongs_to_output_value(
    input_values: Vec<u64>,
    out_values: &mut [u64],
  ) -> HashMap<usize, Vec<u64>> {
    let mut res = input_values
      .iter()
      .enumerate()
      .map(|(idx, _)| (idx, vec![]))
      .collect::<HashMap<_, _>>();
    let mut out_values_current_idx = 0;
    for (pre_idx, mut pre_v) in input_values.into_iter().enumerate() {
      for (idx, v) in out_values.to_owned().into_iter().enumerate() {
        let mut remains = v;
        if idx < out_values_current_idx {
          res.get_mut(&pre_idx).unwrap().push(0);
          continue;
        }
        if pre_v > v {
          res.get_mut(&pre_idx).unwrap().push(v);
          pre_v -= v;
          remains = 0;
        } else {
          res.get_mut(&pre_idx).unwrap().push(pre_v);
          remains -= pre_v;
          pre_v = 0;
        }

        println!("{}", remains);

        if remains == 0 {
          out_values_current_idx += 1;
        }
        out_values[idx] = remains;
      }
    }

    res
  }

  fn outputs_sub_fee(outputs: &mut Vec<(TxOut, Address)>, mut fee: u64) {
    outputs.reverse();
    for output in &mut *outputs {
      if output.0.value >= fee {
        output.0.value -= fee;
        fee = 0;
      } else {
        fee -= output.0.value;
        output.0.value = 0;
      }
    }
    outputs.reverse();
  }

  fn get_pre_outputs(
    tx: &Transaction,
    index: Arc<Index>,
    chain: Chain,
  ) -> (Vec<(TxOut, Address)>, u64) {
    let mut res = vec![];
    for input in tx.input.iter() {
      if let Ok(Some(pre_tx)) = index.get_transaction(input.previous_output.txid) {
        if let Some(pre_out) = pre_tx.output.get(input.previous_output.vout as usize) {
          if let Ok(address) =
            Address::from_script(pre_out.script_pubkey.as_script(), chain.network())
          {
            res.push((pre_out.clone(), address));
          }
        };
      };
    }

    let total_input_value = res.iter().fold(0, |sum, out| sum + out.0.value);
    (res, total_input_value)
  }

  async fn transactions_filter(
    index: Arc<Index>,
    chain: Chain,
    height: u64,
    sellers: Vec<Address>,
  ) -> RpcServerResult<HashMap<Address, Vec<TransactionsFilterResponse>>> {
    // init response
    let response_temp = Arc::new(RwLock::new(
      sellers
        .clone()
        .into_iter()
        .map(|address| (address, vec![]))
        .collect::<HashMap<_, _>>(),
    ));
    log::info!(
      "transactions_filter sellers: {:?}",
      response_temp.read().keys()
    );
    let block_hash = index
      .get_block_hash_by_height(Height(height), None)?
      .ok_or_not_found(|| format!("block_hash({})", height))?;
    let block = index
      .get_block_by_hash(block_hash)?
      .ok_or_not_found(|| format!("block {}", block_hash))?;
    let mut handles = vec![];
    for (idx, tx) in block.txdata.iter().enumerate() {
      let response_temp = response_temp.clone();
      let index_cloned = index.clone();
      let txid = tx.txid();
      let tx = tx.clone();
      handles.push(tokio::spawn(async move {
        let total_output_value = tx.output.iter().fold(0, |sum, out| sum + out.value);
        let mut previous_outs = vec![];
        let mut total_input_value = 0;
        let mut fee = 0;
        let mut out_values = tx.output.iter().map(|out| out.value).collect::<Vec<_>>();
        let mut pre_outputs_belongs_to_output_value = HashMap::new();
        tx.output.iter().enumerate().for_each(|(out_idx, output)| {
          match Address::from_script(output.script_pubkey.as_script(), chain.network()) {
            Ok(seller) => {
              if response_temp.read().contains_key(&seller) {
                if previous_outs.is_empty() {
                  (previous_outs, total_input_value) =
                    Self::get_pre_outputs(&tx, index_cloned.clone(), chain);
                  fee = total_input_value - total_output_value;
                  Self::outputs_sub_fee(&mut previous_outs, fee);
                  pre_outputs_belongs_to_output_value = Self::pre_outputs_belongs_to_output_value(
                    previous_outs
                      .iter()
                      .map(|(out, _)| out.value)
                      .collect::<Vec<_>>(),
                    &mut out_values,
                  );
                }

                if !previous_outs.is_empty() {
                  let mut guard = response_temp.write();
                  if let Some(data) = guard.get_mut(&seller) {
                    previous_outs
                      .iter()
                      .enumerate()
                      .for_each(|(pre_out_idx, (_pre_out, from))| {
                        let value = pre_outputs_belongs_to_output_value
                          .get(&pre_out_idx)
                          .ok_or_else(|| {
                            Error::msg("should find pre_outputs_belongs_to_output_value")
                          })
                          .unwrap()
                          .get(out_idx)
                          .ok_or_else(|| Error::msg("should find value"))
                          .unwrap();

                        data.push((
                          idx,
                          TransactionsFilterResponse {
                            height: Height(height),
                            txid,
                            amount: *value,
                            from: from.clone(),
                            to: seller.clone(),
                          },
                        ));
                      })
                  }
                }
              }
            }
            Err(e) => {
              log::error!("address from_script failed with error: {}", e);
            }
          }
        })
      }))
    }

    for h in handles {
      if let Err(e) = h.await {
        log::error!("{}", e);
      }
    }

    let mut guard = response_temp.write();
    guard
      .iter_mut()
      .for_each(|(_, data)| data.sort_by(|(idx1, _), (idx2, _)| idx1.cmp(idx2)));

    let mut resp = HashMap::new();
    sellers.into_iter().for_each(|seller| {
      if let Some(txs) = guard.remove(&seller) {
        resp.insert(
          seller,
          txs.into_iter().map(|(_, tx)| tx).collect::<Vec<_>>(),
        );
      }
    });

    Ok(resp)
  }

  async fn block_height(index: Arc<Index>) -> RpcServerResult<u64> {
    Ok(index.block_height()?.ok_or_not_found(|| "blockheight")?.n())
  }

  async fn block_time(index: Arc<Index>, height: u64) -> RpcServerResult<u32> {
    let block_hash = index
      .get_block_hash_by_height(Height(height), None)?
      .ok_or_not_found(|| format!("block hash ({})", height))?;
    Ok(
      index
        .block_header(block_hash)?
        .ok_or_else(|| RpcServerError::NotFound(format!("block hash ({})", height)))?
        .time,
    )
  }

  async fn inscriptions_on_output_record(
    index: Arc<Index>,
    outpoint: OutPoint,
  ) -> RpcServerResult<Vec<InscriptionId>> {
    let rtx = index.rtx()?;
    Ok(
      index
        .get_inscriptions_on_output_record(outpoint, &rtx)
        .map_err(RpcServerError::Internal)?
        .into_keys()
        .collect(),
    )
  }

  async fn outpoint_range(index: Arc<Index>, outpoint: OutPoint) -> RpcServerResult<(u64, u64)> {
    let data = index
      .get_outpoint_range(outpoint)?
      .ok_or_not_found(|| format!("outpoint range {outpoint}"))?;

    if let [b0, b1, b2, b3, b4, b5, b6, b7, b8, b9, b10] = data[..] {
      let raw_base = u64::from_le_bytes([b0, b1, b2, b3, b4, b5, b6, 0]);

      // 51 bit base
      let base = raw_base & ((1 << 51) - 1);

      let raw_delta = u64::from_le_bytes([b6, b7, b8, b9, b10, 0, 0, 0]);

      // 33 bit delta
      let delta = raw_delta >> 3;

      Ok((base, base + delta))
    } else {
      Err(RpcServerError::Internal(Error::msg("got invalid range")))
    }
  }

  async fn get_tx_index(
    index: Arc<Index>,
    blockhash: BlockHash,
    txid: Txid,
  ) -> RpcServerResult<Option<u32>> {
    let rtx = index.rtx()?;
    index
      .get_index_on_blockhash_txid(BlockHashWithTxid::new(blockhash, txid), &rtx)
      .map_err(RpcServerError::from)
  }

  async fn inscription_content(
    index: Arc<Index>,
    inscription_id: InscriptionId,
  ) -> RpcServerResult<InscriptionRpcContent> {
    let inscription = index
      .get_inscription_by_id(inscription_id)
      .map_err(|e| {
        log::error!("{}", e);
        e
      })?
      .ok_or_not_found(|| format!("inscription {inscription_id}"))?;
    prepare_content(&inscription)
  }

  async fn inscription_data(
    index: Arc<Index>,
    inscription_id: InscriptionId,
  ) -> RpcServerResult<InscriptionData> {
    let ins = index
      .get_inscription_by_id(inscription_id)
      .map_err(|e| {
        log::error!("{}", e);
        e
      })?
      .ok_or_not_found(|| format!("inscription {inscription_id}"))?;

    let mut body = None;
    let mut content_type = None;
    if let Some(in_body) = ins.body() {
      body = Some(in_body.to_vec())
    }

    if let Some(in_content_type) = ins.content_type() {
      content_type = Some(in_content_type.as_bytes().to_vec())
    }

    Ok(InscriptionData { body, content_type })
  }

  async fn inscription(
    index: Arc<Index>,
    chain: Chain,
    inscription_id: InscriptionId,
  ) -> RpcServerResult<InscriptionRaw> {
    let tx = index
      .get_transaction(inscription_id.txid)
      .map_err(|e| {
        log::error!("{}", e);
        e
      })?
      .ok_or_not_found(|| format!("inscription {inscription_id}"))?;

    let total = ParsedEnvelope::from_transaction(&tx);

    if total.is_empty() || total.len() - 1 < inscription_id.index as usize {
      return Err(RpcServerError::NotFound(format!(
        "inscription {inscription_id}"
      )));
    }

    let ti = total
      .into_iter()
      .nth(inscription_id.index as usize)
      .ok_or_not_found(|| format!("inscription {inscription_id}"))?;

    let entry = index
      .get_inscription_entry(inscription_id)
      .map_err(|e| {
        log::error!("{}", e);
        e
      })?
      .ok_or_not_found(|| format!("inscription {inscription_id}"))?;

    let satpoint = index
      .get_inscription_satpoint_by_id(inscription_id)
      .map_err(|e| {
        log::error!("{}", e);
        e
      })?
      .ok_or_not_found(|| format!("inscription {inscription_id}"))?;

    let output = if satpoint.outpoint == unbound_outpoint() {
      None
    } else {
      Some(
        index
          .get_transaction(satpoint.outpoint.txid)
          .map_err(|e| {
            log::error!("{}", e);
            e
          })?
          .ok_or_not_found(|| format!("inscription {inscription_id} current transaction"))?
          .output
          .into_iter()
          .nth(satpoint.outpoint.vout.try_into().unwrap())
          .ok_or_not_found(|| format!("inscription {inscription_id} current transaction output"))?,
      )
    };

    let previous = if let Some(n) = entry.sequence_number.checked_sub(1) {
      index.get_inscription_id_by_sequence_number(n)?
    } else {
      None
    };

    let next = index.get_inscription_id_by_sequence_number(entry.sequence_number + 1)?;

    let body = if let Some(body_u8) = ti.payload.body.clone() {
      base64::engine::general_purpose::STANDARD_NO_PAD.encode(body_u8)
    } else {
      Default::default()
    };

    Ok(InscriptionRaw {
      chain,
      genesis_fee: entry.fee,
      genesis_height: entry.height,
      genesis_transaction: inscription_id.txid,
      content: InscriptionRpcContent {
        body,
        content_type: ti.payload.content_type().unwrap_or_default().to_string(),
      },
      inscription_id,
      tx_in_index: ti.input,
      tx_in_offset: ti.offset,
      next,
      number: entry.inscription_number,
      output,
      previous,
      sat: entry.sat,
      sat_point: satpoint,
      timestamp: timestamp(entry.timestamp),
    })
  }

  async fn getrawtransaction(txid: Txid, index: Arc<Index>) -> RpcServerResult<Transaction> {
    index
      .get_transaction(txid)
      .map_err(|e| {
        log::error!("{}", e);
        e
      })?
      .ok_or_not_found(|| format!("transaction {txid}"))
  }

  async fn single_transaction(
    tx_raw: &GetRawTransactionResult,
    tx: &Transaction,
    chain: Chain,
    index: Arc<Index>,
    pool: Arc<rayon::ThreadPool>,
    txid: Txid,
    should_log_details: bool,
    uid: Uuid,
  ) -> RpcServerResult<Vec<InscriptionOnTx>> {
    let transaction_inscriptions = Arc::new(Mutex::new(Vec::new()));
    if !tx_raw.vout.is_empty() {
      let mut handles = vec![];
      // check if has inscription location history
      let vouts = tx_raw.vout.clone();
      let total_num = vouts.len();
      let complete = Arc::new(AtomicU64::new(0));
      let tx_input_infos = Arc::new(TransactionInputInfo::extract_from_tx(&tx, &index, chain)?);
      for o in vouts {
        let index_cloned = index.clone();
        let pool_cloned = pool.clone();
        // let txins = tx.input.clone();
        let tx_input_infos = tx_input_infos.clone();
        let transaction_inscriptions_cloned = transaction_inscriptions.clone();
        let complete = complete.clone();
        handles.push(tokio::spawn(async move {
          pool_cloned.install(|| {
            conditional_log!(
              should_log_details,
              "uid: {}, method: single_transaction, txid: {}, vout: {}/{} start",
              uid,
              txid,
              o.n + 1,
              total_num
            );
            let rtx = index_cloned.rtx()?;
            let ids = index_cloned
              .get_inscriptions_on_output_record(OutPoint { txid, vout: o.n }, &rtx)
              .map_err(|e| {
                log::error!("{}", e);
                e
              })?;

            for (offset, id) in ids.into_iter().enumerate() {
              let mut start = Instant::now();
              let in_raw = index_cloned
                .get_inscription_by_id_with_rtx(id.0, &rtx)?
                .ok_or_not_found(|| format!("inscription {}", id.0))?;
              conditional_log!(
                should_log_details,
                "uid: {}, method: single_transaction, txid: {}, vout: {}/{} get inscription raw cost {:?}",
                uid,
                txid,
                o.n + 1,
                total_num,
                Instant::now().saturating_duration_since(start)
              );

              if in_raw.media() != Media::Text {
                continue;
              }

              let mut from = None;
              start = Instant::now();
              for (_pre_previous_outputtxid, txin_info) in tx_input_infos.iter() {
                if txin_info.outpoint_inscriptions.contains_key(&id.0) || {
                  if let Some(tapscript) = txin_info.witness.tapscript() {
                    if let Ok(input_envelopes) = RawEnvelope::from_tapscript(tapscript, txin_info.tx_in_index) {
                      input_envelopes.into_iter().find(|i| ParsedEnvelope::from(i.clone()).payload == in_raw).is_some()
                    }else {
                      false
                    }
                  }else {
                    false
                  }
                } {
                  from = txin_info.outpoint_address.clone();
                  break;
                }
              }

              conditional_log!(
                should_log_details,
                "uid: {}, method: single_transaction, txid: {}, vout: {}/{} find from address cost {:?}",
                uid,
                txid,
                o.n + 1,
                total_num,
                Instant::now().saturating_duration_since(start)
              );

              let entry = index_cloned
                .get_inscription_entry_with_rtx(id.0, &rtx)?
                .ok_or_not_found(|| format!("inscription {}", id.0))?;

              let content = prepare_content(&in_raw)?;

              transaction_inscriptions_cloned
                .lock()
                .push(InscriptionOnTx {
                  inscription_id: id.0,
                  content,
                  idx: u32::try_from(offset)
                    .map_err(|e| RpcServerError::Internal(Error::msg(e)))
                    .map_err(|e| {
                      log::error!("{}", e);
                      e
                    })?,
                  vout: o.n,
                  number: entry.inscription_number,
                  output_on_tx: OutPoint { txid, vout: o.n },
                  sat_point_on_tx: SatPoint {
                    outpoint: OutPoint { txid, vout: o.n },
                    offset: offset as u64,
                  },
                  genesis_transaction: id.0.txid,
                  from,
                  to: o
                    .script_pub_key
                    .address
                    .clone()
                    .map(|address| address.assume_checked()),
                });
            }
            conditional_log!(
              should_log_details,
              "uid: {}, method: single_transaction, txid: {}, vout: {}/{} done ({}/{})",
              uid,
              txid,
              o.n + 1,
              total_num,
              complete.fetch_add(1, Ordering::Relaxed) + 1,
              total_num
            );
            Ok::<(), RpcServerError>(())
          })
        }))
      }

      let handles_num = handles.len();
      conditional_log!(
        should_log_details,
        "uid: {}, method: single_transaction, txid: {}, wait handles(num: {})",
        txid,
        uid,
        handles_num
      );
      for (idx, h) in handles.into_iter().enumerate() {
        h.await
          .map_err(Error::from)
          .map_err(RpcServerError::from)??;
        conditional_log!(
          should_log_details,
          "uid: {}, method: single_transaction, txid: {}, wait handles {}/{} done",
          txid,
          uid,
          idx + 1,
          handles_num
        );
      }
      conditional_log!(
        should_log_details,
        "uid: {}, method: single_transaction, txid: {}, wait handles done totally",
        txid,
        uid
      );
    }

    let mut res = transaction_inscriptions.lock().clone();
    res.sort_by(|a, b| a.vout.cmp(&b.vout));

    Ok(res)
  }

  async fn transaction(
    chain: Chain,
    index: Arc<Index>,
    pool: Arc<rayon::ThreadPool>,
    txids: Vec<Txid>,
    show_tx_detail: bool,
  ) -> RpcServerResult<Vec<TransactionRpc>> {
    let uid = uuid::Uuid::new_v4();
    let total_num = txids.len();
    let complete_num = Arc::new(AtomicU64::new(0));
    let should_log_details = LOG_DETAILS.load(atomic::Ordering::Relaxed);
    let transaction_rpcs = Arc::new(Mutex::new(IndexMap::new()));
    let mut handles = vec![];
    let txids_len = txids.len();
    for (txid_idx, txid) in txids.into_iter().enumerate() {
      let pool_cloned = pool.clone();
      let index_cloned = index.clone();
      let transaction_rpcs = transaction_rpcs.clone();
      let complete_num = complete_num.clone();
      handles.push(tokio::spawn(async move {
        let tx_raw = index_cloned.get_raw_transaction_info(txid).map_err(|e| {
          log::error!("{}", e);
          e
        })?;

        let tx = if tx_raw.is_coinbase() {
          index_cloned
            .get_transaction(txid)
            .map_err(|e| {
              log::error!("{}", e);
              e
            })?
            .ok_or_not_found(|| format!("transaction {txid}"))?
        } else {
          tx_from_raw(&tx_raw)?
        };

        conditional_log!(
          should_log_details,
          "uid: {}, method: transaction, txid: {}, output_num: {}, start",
          uid,
          txid,
          tx.output.len()
        );

        let transaction_inscriptions = Self::single_transaction(
          &tx_raw,
          &tx,
          chain,
          index_cloned.clone(),
          pool_cloned,
          txid,
          should_log_details,
          uid,
        )
        .await?;

        let mut height = None;
        let mut blockhash = None;
        let mut txidx = None;

        let rtx = index_cloned.rtx()?;
        if let Some(blockhash_raw) = tx_raw.blockhash {
          height = index_cloned.get_block_height_by_hash(blockhash_raw, &rtx)?;
          txidx = index_cloned
            .get_index_on_blockhash_txid(BlockHashWithTxid::new(blockhash_raw, txid), &rtx)?;
          blockhash = Some(blockhash_raw)
        }

        transaction_rpcs.lock().insert(
          txid_idx,
          TransactionRpc {
            chain,
            height,
            block_hash: blockhash,
            block_time: tx_raw.blocktime,
            txid,
            txidx,
            time: tx_raw.time,
            transaction: if show_tx_detail { Some(tx) } else { None },
            confirmations: tx_raw.confirmations.unwrap_or_default(),
            inscriptions_ontx: transaction_inscriptions,
          },
        );

        conditional_log!(
          should_log_details,
          "uid: {}, method: transaction, txid: {}, done ({}/{})",
          uid,
          txid,
          complete_num.fetch_add(1, Ordering::Relaxed) + 1,
          total_num
        );
        Ok::<(), RpcServerError>(())
      }));
    }

    let handles_num = handles.len();
    conditional_log!(
      should_log_details,
      "uid: {}, method: transaction, wait handles(num: {})",
      uid,
      handles_num
    );
    for (idx, h) in handles.into_iter().enumerate() {
      h.await
        .map_err(Error::from)
        .map_err(RpcServerError::from)??;
      conditional_log!(
        should_log_details,
        "uid: {}, method: transaction, wait handles {}/{} done",
        uid,
        idx + 1,
        handles_num
      );
    }
    conditional_log!(
      should_log_details,
      "uid: {}, method: transaction, wait handles done totally",
      uid
    );

    conditional_log!(should_log_details, "uid: {}, method: transaction done", uid);

    let mut list_guard = transaction_rpcs.lock();
    if list_guard.len() != txids_len {
      return Err(RpcServerError::Internal(Error::msg(format!(
        "result len not met the txids len {}",
        txids_len
      ))));
    }

    let mut resp = vec![];
    list_guard.sort_by(|k1, _, k2, _| k1.cmp(k2));
    while !list_guard.is_empty() {
      if let Some(txid) = list_guard.shift_remove_index(0) {
        resp.push(txid.1)
      }
    }

    Ok(resp)
  }

  pub(crate) async fn txids_by_height(
    index: Arc<Index>,
    height: Height,
    pool: Arc<rayon::ThreadPool>,
    only_show_ins: bool,
  ) -> RpcServerResult<Vec<Txid>> {
    let rtx = index.rtx()?;
    let block_hash = index
      .get_block_hash_by_height(height, None)?
      .ok_or_not_found(|| format!("block_hash: height {}", height))?;
    let block = index
      .get_block_by_hash(block_hash)?
      .ok_or_not_found(|| format!("block: block_hash {}", block_hash))?;
    //find coinbase txid
    let coinbase_txid = block
      .txdata
      .iter()
      .find(|tx| tx.is_coin_base())
      .map(|tx| tx.txid());
    // collect txids except coinbase_txid
    let mut txids = vec![];
    for tx in block.txdata.iter() {
      if let Some(coinbase_txid) = coinbase_txid {
        if !tx.txid().eq(&coinbase_txid) {
          txids.push(tx.txid());
        }
      } else {
        txids.push(tx.txid());
      }
    }
    // push coin_base_txid to last
    if let Some(coinbase_txid) = coinbase_txid {
      txids.push(coinbase_txid);
    }

    let response = if only_show_ins {
      let list = Arc::new(Mutex::new(IndexMap::new()));

      let tx_output_nums = index.get_txid_output_num(&txids, &rtx)?;

      if tx_output_nums.len() != txids.len() {
        return Err(RpcServerError::Internal(Error::msg(format!(
          "tx_output_nums len {} not eq txids len {}",
          tx_output_nums.len(),
          txids.len()
        ))));
      }

      let mut handles = vec![];

      for (n, id) in txids.into_iter().enumerate() {
        if INDEX_CACHE.text_txids.read().contains_key(&id) {
          list.lock().insert(n, id);
        } else {
          let tx_output_num = tx_output_nums
            .get(n)
            .ok_or_not_found(|| format!("tx_output_num {}", id))?
            .to_owned();

          let cloned_pool = pool.clone();
          let cloned_index = index.clone();
          let list = list.clone();
          let old_idx = n;
          handles.push(tokio::spawn(async move {
            cloned_pool.install(|| {
              let rtx = cloned_index.rtx()?;
              for idx in 0..tx_output_num {
                let record = cloned_index.get_inscriptions_on_output_record(
                  OutPoint {
                    txid: id,
                    vout: idx,
                  },
                  &rtx,
                )?;

                if !record.is_empty() && has_text_inscription(cloned_index.clone(), record, &rtx)? {
                  list.lock().insert(old_idx, id);
                  break;
                }
              }
              Ok::<(), Error>(())
            })
          }))
        }
      }

      for handle in handles {
        handle
          .await
          .map_err(Error::from)
          .map_err(RpcServerError::from)??
      }

      let mut list_guard = list.lock();
      let mut resp = vec![];
      list_guard.sort_by(|k1, _, k2, _| k1.cmp(k2));
      while !list_guard.is_empty() {
        if let Some(txid) = list_guard.shift_remove_index(0) {
          resp.push(txid.1)
        }
      }
      resp
    } else {
      txids
    };

    // cache next
    // let index_cloned = index.clone();
    // thread::spawn(move || {
    //   index_cloned.cache_height(height.add(1));
    // });

    Ok(response)
  }
}

fn prepare_content(inscription: &Inscription) -> RpcServerResult<InscriptionRpcContent> {
  let body = if let Some(body_u8) = inscription.body() {
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(body_u8)
  } else {
    Default::default()
  };
  let content_type = inscription.content_type().unwrap_or_default().to_string();
  Ok(InscriptionRpcContent { body, content_type })
}

fn has_text_inscription(
  index: Arc<Index>,
  mut record: HashMap<InscriptionId, ()>,
  rtx: &ReadTransaction,
) -> Result<bool> {
  for (id, _) in record.drain() {
    if INDEX_CACHE.text_inscription_ids.read().contains_key(&id) {
      return Ok(true);
    } else if let Some(ins) = index.get_inscription_by_id_with_rtx(id, rtx)? {
      if let Some(cts) = ins.content_type() {
        if let Ok(media) = Media::from_str(cts) {
          if media == Media::Text {
            return Ok(true);
          }
        }
      }
    }
  }
  Ok(false)
}

fn tx_from_raw(tx_raw: &GetRawTransactionResult) -> RpcServerResult<Transaction> {
  let mut inputs = vec![];
  let mut outputs = vec![];
  for (idx, txin) in tx_raw.vin.iter().enumerate() {
    let vout = txin
      .vout
      .ok_or_not_found(|| format!("vout: previous_output index {} in {}", idx, tx_raw.txid))?;
    // if idx != vout as usize {
    //   return Err(RpcServerError::Internal(Error::msg(format!(
    //     "input idx {} not met vout {} in {}",
    //     idx, vout, tx_raw.txid
    //   ))));
    // };
    inputs.insert(
      idx,
      TxIn {
        previous_output: OutPoint {
          txid: txin
            .txid
            .ok_or_not_found(|| format!("previous_output txid: in {}", tx_raw.txid))?,
          vout,
        },
        script_sig: txin
          .script_sig
          .clone()
          .ok_or_not_found(|| format!("script_sig in {}", tx_raw.txid))?
          .script()
          .map_err(|e| Error::msg(e.to_string()))
          .map_err(RpcServerError::from)?,
        sequence: Sequence(txin.sequence),
        witness: if txin.txinwitness.is_none() {
          bitcoin::Witness::default()
        } else {
          bitcoin::Witness::from(txin.txinwitness.clone().unwrap())
        },
      },
    )
  }

  for (idx, txout) in tx_raw.vout.iter().enumerate() {
    if idx != txout.n as usize {
      return Err(RpcServerError::Internal(Error::msg(format!(
        "output idx {} not met vout {} in {}",
        idx, txout.n, tx_raw.txid
      ))));
    };
    outputs.insert(
      idx,
      TxOut {
        value: txout.value.to_sat(),
        script_pubkey: txout
          .script_pub_key
          .script()
          .map_err(|e| Error::msg(e.to_string()))
          .map_err(RpcServerError::from)?,
      },
    )
  }

  Ok(Transaction {
    version: tx_raw.version as i32,
    lock_time: absolute::LockTime::from_consensus(tx_raw.locktime),
    input: inputs,
    output: outputs,
  })
}

#[cfg(test)]
mod test {
  use super::*;
  use crate::test::Chain::Mainnet;
  use bitcoin::absolute::LockTime;
  use bitcoin::psbt::PsbtSighashType;
  use bitcoin::sighash::EcdsaSighashType;
  use bitcoincore_rpc::json::SigHashType;
  use bitcoincore_rpc::Auth;
  use jsonrpsee_core::client::ClientT;
  use jsonrpsee_core::rpc_params;
  use jsonrpsee_http_client::HttpClientBuilder;

  fn init_logger() {
    Builder::new()
      .format(|buf, record| {
        let mut level_style = buf.style();
        level_style.set_color(env_logger::fmt::Color::Magenta);
        let level = level_style.value(record.level());

        let file = record.file().unwrap_or("<unknown>");
        let line = record.line().unwrap_or(0);
        let location = format!("{}:{}", file, line);
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

        writeln!(
          buf,
          "[{timestamp} {level} {} - {location}] {}",
          record.module_path().unwrap_or("<unknown>"),
          record.args()
        )
      })
      .filter(None, LevelFilter::Info)
      .parse_env(Env::from(DEFAULT_FILTER_ENV))
      .init();
  }

  #[test]
  fn test_log() {
    init_logger();
    log::debug!("aaaaaa")
  }

  #[tokio::test]
  async fn test_inscription() {
    let url = env::var("ORD_URL").unwrap();
    let client = Arc::new(
      HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(300))
        .build(&url)
        .unwrap(),
    );
    let x: InscriptionRaw = client
      .request(
        "inscription",
        rpc_params!["67afd9ba948334eb33875846a990d89a41091981bc3ff0b902eea69c43f69b7ei0"],
      )
      .await
      .unwrap();

    println!("{:?}", x);

    println!("response content: {}", x.content.body);

    let data: [u8; 10] = [89, 100, 76, 54, 69, 67, 50, 83, 50, 103];

    let base64_s = base64::engine::general_purpose::STANDARD_NO_PAD.encode(data);
    println!("base64_s content from bytes: {}", base64_s);
    println!(
      "response content equal with base64_s: {}",
      x.content.body.eq(&base64_s)
    );

    let result = String::from_utf8(data.to_vec()).unwrap();
    println!("content from bytes: {}", result);
    let recovered = base64::engine::general_purpose::STANDARD_NO_PAD
      .decode(base64_s)
      .unwrap();
    println!("content recovered from base64: {:?}", recovered);
    println!(
      "raw data equal with content recovered from base64: {}",
      data.to_vec().eq(&recovered)
    );
  }

  #[test]
  fn test_tx_from_raw() {
    let url = env::var("BTC_URL").unwrap();
    let auth = Auth::UserPass("user".to_string(), "pass".to_string());
    let client = Client::new(&url, auth)
      .with_context(|| format!("failed to connect to Bitcoin Core RPC at {url}"))
      .unwrap();
    let txid =
      &Txid::from_str("a174e26a92e9601057ddc2c86d856eb38be92f0d7fe7f796fa99afef187135af").unwrap();
    let tx = client.get_raw_transaction(&txid, None).unwrap();
    let tx_raw = client.get_raw_transaction_info(&txid, None).unwrap();
    assert_eq!(tx.input.len(), tx_raw.vin.len());
    assert_eq!(tx.output.len(), tx_raw.vout.len());
    assert_eq!(tx.version, tx_raw.version as i32);
    assert_eq!(tx.lock_time.to_consensus_u32(), tx_raw.locktime);
    assert_eq!(
      tx.lock_time,
      absolute::LockTime::from_consensus(tx_raw.locktime)
    );
    for (idx, raw_vin) in tx_raw.vin.iter().enumerate() {
      let tx_vin = tx.input[idx].clone();
      assert_eq!(tx_vin.previous_output.vout, raw_vin.vout.unwrap());
      assert_eq!(tx_vin.previous_output.txid, raw_vin.txid.unwrap());
      assert_eq!(tx_vin.sequence.0, raw_vin.sequence);
      assert_eq!(
        tx_vin.script_sig,
        raw_vin.script_sig.clone().unwrap().script().unwrap()
      );
      let raw_witness = Witness::from(raw_vin.txinwitness.clone().unwrap());
      assert_eq!(tx_vin.witness, raw_witness)
    }

    for (idx, raw_vout) in tx_raw.vout.iter().enumerate() {
      let tx_vout = tx.output[idx].clone();
      assert_eq!(tx_vout.value, raw_vout.value.to_sat());
      assert_eq!(
        tx_vout.script_pubkey,
        raw_vout.script_pub_key.script().unwrap()
      );
      assert_eq!(idx, raw_vout.n as usize)
    }

    let tx_recover = tx_from_raw(&tx_raw).unwrap();
    assert_eq!(tx, tx_recover)
  }

  #[tokio::test]
  async fn test_txids_height() {
    let url = env::var("ORD_URL").unwrap();
    let client = Arc::new(
      HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(300))
        .build(&url)
        .unwrap(),
    );
    let mut handles = vec![];
    let start_out = Instant::now();
    let x: Vec<String> = client
      .request("txids_by_height", rpc_params![804300, true])
      .await
      .unwrap();
    for _ in 0..10 {
      let client = client.clone();
      let x = x.clone();
      handles.push(tokio::spawn(async move {
        let start = Instant::now();
        let _: Vec<TransactionRpc> = client.request("transaction", x).await.unwrap();
        println!("cost {:?}", Instant::now().duration_since(start));
      }));
    }

    for handle in handles {
      let _ = handle.await;
    }

    println!(
      "totally cost {:?}",
      Instant::now().duration_since(start_out)
    );
  }

  #[tokio::test]
  async fn test_transaction() {
    let url = env::var("ORD_URL").unwrap();
    let client = Arc::new(
      HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(3600))
        .max_response_size(TEN_MB_SIZE_BYTES * 100)
        .build(&url)
        .unwrap(),
    );

    let start = Instant::now();
    let txs: Vec<TransactionRpc> = client
      .request(
        "transaction",
        ["a27b59d602fe19664e94fc32221612bb84428ad8aee38c3f45df19d50024b131"],
      )
      .await
      .unwrap();
    println!("cost {:?}", Instant::now().saturating_duration_since(start));
    println!("inscriptions_len: {}", txs[0].inscriptions_ontx.len())
  }

  #[tokio::test]
  async fn test_tx() {
    let url1 = env::var("ORD_URL1").unwrap();
    let client1 = Arc::new(
      HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(300))
        .build(&url1)
        .unwrap(),
    );

    let url2 = env::var("ORD_URL2").unwrap();
    let client2 = Arc::new(
      HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(300))
        .build(&url2)
        .unwrap(),
    );

    let ids1: Vec<String> = client1
      .request("txids_by_height", rpc_params![797770, true])
      .await
      .unwrap();

    let ids2: Vec<String> = client2
      .request("txids_by_height", rpc_params![797770, true])
      .await
      .unwrap();

    assert_eq!(ids1, ids2);

    let txs1: Vec<TransactionRpc> = client1.request("transaction", ids1).await.unwrap();
    let txs2: Vec<TransactionRpc> = client1.request("transaction", ids2).await.unwrap();
    for tx in &txs2 {
      if !txs1.contains(tx) {
        println!("{:?}", tx)
      }
    }

    assert_eq!(txs1.len(), txs2.len());
    assert_eq!(txs1, txs2)
  }

  #[test]
  fn test_address() {
    let result =
      Address::from_str("bc1pga0879hg05nveddhk7yzvphevt3xxutes6racyhtxz37w7hc86tscn2v9r").unwrap();
    println!("{}", result.assume_checked())
  }

  #[test]
  fn test_address_from_script() {
    let url = env::var("BTC_URL").unwrap();
    let auth = Auth::UserPass("user".to_string(), "pass".to_string());
    let client = Client::new(&url, auth)
      .with_context(|| format!("failed to connect to Bitcoin Core RPC at {url}"))
      .unwrap();
    let txid =
      &Txid::from_str("70993ed523748baf9449cf0a1ae537e29ef52d0acdb73053e15a5d76827510c8").unwrap();
    let tx = client.get_raw_transaction(&txid, None).unwrap();
    let address =
      Address::from_script(tx.output[0].script_pubkey.as_script(), Mainnet.network()).unwrap();
    println!("address: {}", address)
  }

  #[tokio::test]
  async fn test_transactions_filter() {
    let url = env::var("BTC_URL").unwrap();
    let auth = Auth::UserPass("user".to_string(), "pass".to_string());
    let client = Arc::new(
      Client::new(&url, auth)
        .with_context(|| format!("failed to connect to Bitcoin Core RPC at {url}"))
        .unwrap(),
    );
    let chain = Mainnet;
    let height = 807154;
    let sellers = vec![Address::from_str("3G5XpAc211rLkaLjKcQdoYXUs46pPaLi4G")
      .unwrap()
      .require_network(chain.network())
      .unwrap()];

    let hash =
      BlockHash::from_str("00000000000000000003fa32c8eef97ea6a2a41e234b007deed39b370103f20b")
        .unwrap();
    let response_temp = Arc::new(RwLock::new(
      sellers
        .clone()
        .into_iter()
        .map(|address| (address, vec![]))
        .collect::<HashMap<_, _>>(),
    ));

    let block = client.get_block(&hash).unwrap();
    let mut handles = vec![];
    for (idx, tx) in block.txdata.iter().enumerate() {
      let response_temp = response_temp.clone();
      let txid = tx.txid();
      let tx = tx.clone();
      let client = client.clone();
      handles.push(tokio::spawn(async move {
        let total_output_value = tx.output.iter().fold(0, |sum, out| sum + out.value);
        let mut previous_outs = vec![];
        let mut total_input_value = 0;
        let mut fee = 0;
        let mut out_values = tx.output.iter().map(|out| out.value).collect::<Vec<_>>();
        let mut pre_outputs_belongs_to_output_value = HashMap::new();
        tx.output.iter().enumerate().for_each(|(out_idx, output)| {
          match Address::from_script(output.script_pubkey.as_script(), chain.network()) {
            Ok(seller) => {
              if response_temp.read().contains_key(&seller) {
                if previous_outs.is_empty() {
                  (previous_outs, total_input_value) = {
                    let mut res = vec![];
                    for input in tx.input.iter() {
                      if let Ok(pre_tx) =
                        client.get_raw_transaction(&input.previous_output.txid, None)
                      {
                        if let Some(pre_out) =
                          pre_tx.output.get(input.previous_output.vout as usize)
                        {
                          if let Ok(address) =
                            Address::from_script(pre_out.script_pubkey.as_script(), chain.network())
                          {
                            res.push((pre_out.clone(), address));
                          }
                        };
                      };
                    }

                    let total_input_value = res.iter().fold(0, |sum, out| sum + out.0.value);
                    (res, total_input_value)
                  };

                  fee = total_input_value - total_output_value;
                  RpcServer::outputs_sub_fee(&mut previous_outs, fee);
                  pre_outputs_belongs_to_output_value =
                    RpcServer::pre_outputs_belongs_to_output_value(
                      previous_outs
                        .iter()
                        .map(|(out, _)| out.value)
                        .collect::<Vec<_>>(),
                      &mut out_values,
                    );
                }

                if !previous_outs.is_empty() {
                  let mut guard = response_temp.write();
                  if let Some(data) = guard.get_mut(&seller) {
                    previous_outs
                      .iter()
                      .enumerate()
                      .for_each(|(pre_out_idx, (_pre_out, from))| {
                        let value = pre_outputs_belongs_to_output_value
                          .get(&pre_out_idx)
                          .ok_or_else(|| {
                            Error::msg("should find pre_outputs_belongs_to_output_value")
                          })
                          .unwrap()
                          .get(out_idx)
                          .ok_or_else(|| Error::msg("should find value"))
                          .unwrap();
                        data.push((
                          idx,
                          TransactionsFilterResponse {
                            height: Height(height),
                            txid,
                            amount: *value,
                            from: from.clone(),
                            to: seller.clone(),
                          },
                        ));
                      })
                  }
                }
              }
            }
            Err(e) => {
              log::error!("address from_script failed with error: {}", e);
            }
          }
        })
      }))
    }

    for h in handles {
      if let Err(e) = h.await {
        log::error!("{}", e);
      }
    }

    let mut guard = response_temp.write();
    guard
      .iter_mut()
      .for_each(|(_, data)| data.sort_by(|(idx1, _), (idx2, _)| idx1.cmp(&idx2)));

    let mut resp = HashMap::new();
    sellers.into_iter().for_each(|seller| {
      if let Some(txs) = guard.remove(&seller) {
        let txs = txs.into_iter().map(|(_, tx)| tx).collect::<Vec<_>>();
        resp.insert(seller, txs);
      }
    });

    println!("{:?}", resp)
  }

  #[tokio::test]
  async fn test_tx_cost() {
    let url1 = env::var("ORD_URL1").unwrap();
    let client1 = Arc::new(
      HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(300))
        .build(&url1)
        .unwrap(),
    );

    let url2 = env::var("ORD_URL2").unwrap();
    let client2 = Arc::new(
      HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(300))
        .build(&url2)
        .unwrap(),
    );

    let mut hs = vec![];

    let mut totally = Instant::now();
    for i in 800020..800030 {
      let client1 = client1.clone();
      hs.push(tokio::spawn(async move {
        let mut start = Instant::now();
        let ids1: Vec<String> = client1
          .request("txids_by_height", rpc_params![i, true])
          .await
          .unwrap();

        println!(
          "clien1 txids_by_height {} cost {:?}",
          i,
          Instant::now().saturating_duration_since(start)
        );

        start = Instant::now();
        let _txs1: Vec<TransactionRpc> = client1.request("transaction", ids1).await.unwrap();
        println!(
          "clien1 transaction {} cost {:?}",
          i,
          Instant::now().saturating_duration_since(start)
        );
      }));
    }

    for h in hs {
      h.await.unwrap();
    }

    println!(
      "{} totally cost {:?}",
      url1,
      Instant::now().saturating_duration_since(totally)
    );

    println!("\n\n\n");

    let mut hs = vec![];
    totally = Instant::now();
    for i in 800020..800030 {
      let client2 = client2.clone();
      hs.push(tokio::spawn(async move {
        let mut start = Instant::now();
        let ids2: Vec<String> = client2
          .request("txids_by_height", rpc_params![i, true])
          .await
          .unwrap();

        println!(
          "client2 txids_by_height {} cost {:?}",
          i,
          Instant::now().saturating_duration_since(start)
        );

        start = Instant::now();
        let _txs2: Vec<TransactionRpc> = client2.request("transaction", ids2).await.unwrap();
        println!(
          "client2 transaction {} cost {:?}",
          i,
          Instant::now().saturating_duration_since(start)
        );
      }));
    }

    for h in hs {
      h.await.unwrap();
    }
    println!(
      "{} totally cost {:?}",
      url2,
      Instant::now().saturating_duration_since(totally)
    );
  }

  #[tokio::test]
  async fn test_tx_cost_sync() {
    let url1 = env::var("ORD_URL1").unwrap();
    let client1 = Arc::new(
      HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(300))
        .build(&url1)
        .unwrap(),
    );

    let url2 = env::var("ORD_URL2").unwrap();
    let client2 = Arc::new(
      HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(300))
        .build(&url2)
        .unwrap(),
    );

    let mut totally = Instant::now();
    for i in 800060..800070 {
      let mut start = Instant::now();
      let ids1: Vec<String> = client1
        .request("txids_by_height", rpc_params![i, true])
        .await
        .unwrap();

      println!(
        "clien1 txids_by_height {} cost {:?}",
        i,
        Instant::now().saturating_duration_since(start)
      );

      start = Instant::now();
      let _txs1: Vec<TransactionRpc> = client1.request("transaction", ids1).await.unwrap();
      println!(
        "clien1 transaction {} cost {:?}",
        i,
        Instant::now().saturating_duration_since(start)
      );
    }

    println!(
      "{} totally cost {:?}",
      url1,
      Instant::now().saturating_duration_since(totally)
    );

    println!("\n\n\n");

    totally = Instant::now();
    for i in 800060..800070 {
      let mut start = Instant::now();
      let ids2: Vec<String> = client2
        .request("txids_by_height", rpc_params![i, true])
        .await
        .unwrap();

      println!(
        "client2 txids_by_height {} cost {:?}",
        i,
        Instant::now().saturating_duration_since(start)
      );

      start = Instant::now();
      let _txs2: Vec<TransactionRpc> = client2.request("transaction", ids2).await.unwrap();
      println!(
        "client2 transaction {} cost {:?}",
        i,
        Instant::now().saturating_duration_since(start)
      );
    }

    println!(
      "{} totally cost {:?}",
      url2,
      Instant::now().saturating_duration_since(totally)
    );
  }

  #[test]
  fn test_psbt_extract_tx() {
    let (psbt_words, expected_tx) = fake_psbt();
    let psbt = Psbt::from_str(&psbt_words).unwrap();
    let recovered_tx = psbt.extract_tx();
    println!("{:?}", recovered_tx);
    for (idx, input) in expected_tx.input.iter().enumerate() {
      assert_eq!(
        input.previous_output,
        recovered_tx.input[idx].previous_output
      )
    }
    for (idx, output) in expected_tx.output.iter().enumerate() {
      assert_eq!(output.script_pubkey, recovered_tx.output[idx].script_pubkey);
      assert_eq!(output.value, recovered_tx.output[idx].value)
    }
  }

  fn fake_psbt() -> (String, Transaction) {
    let url = env::var("BTC_URL").unwrap();
    let auth = Auth::UserPass(env::var("BTC_USER").unwrap(), env::var("BTC_PASS").unwrap());
    let client = Arc::new(
      Client::new(&url, auth.clone())
        .with_context(|| format!("failed to connect to Bitcoin Core RPC at {url}"))
        .unwrap(),
    );

    let wallet_url = env::var("BTC_WALLET_URL").unwrap();
    let wallet_client = Arc::new(
      Client::new(&wallet_url, auth)
        .with_context(|| format!("failed to connect to Bitcoin Core RPC at {wallet_url}"))
        .unwrap(),
    );

    let inscription_utxo =
      OutPoint::from_str("0a3d823653409bd9a1f36b2012fdff3b49bb5f1d1fc95aa407534c19231de2e2:0")
        .unwrap();
    let tx = client
      .get_raw_transaction(&inscription_utxo.txid, None)
      .unwrap();

    let inscription_output = tx.output[inscription_utxo.vout as usize].clone();

    let tx_new = Transaction {
      version: 2,
      lock_time: LockTime::ZERO,
      input: vec![TxIn {
        previous_output: OutPoint {
          txid: inscription_utxo.txid,
          vout: inscription_utxo.vout,
        },
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::default(),
      }],
      output: vec![TxOut {
        value: 1000,
        script_pubkey: inscription_output.script_pubkey,
      }],
    };

    let mut psbt = Psbt::from_unsigned_tx(tx_new.clone()).unwrap();

    psbt.inputs[0].non_witness_utxo = Some(tx.clone());
    psbt.inputs[0].sighash_type = Some(PsbtSighashType::from(
      EcdsaSighashType::SinglePlusAnyoneCanPay,
    ));

    let processed_seller_psbt = wallet_client
      .wallet_process_psbt(
        &psbt.to_string(),
        Some(true),
        Some(SigHashType::from(EcdsaSighashType::SinglePlusAnyoneCanPay)),
        None,
      )
      .unwrap();

    (processed_seller_psbt.psbt, tx_new)
  }
}

#[derive(Clone, Serialize)]
struct InscriptionData {
  body: Option<Vec<u8>>,
  content_type: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize)]
struct TransactionsFilterResponse {
  height: Height,
  txid: Txid,
  amount: u64,
  from: Address,
  to: Address,
}

struct TransactionInputInfo {
  tx_in_index: usize,
  witness: bitcoin::Witness,
  outpoint_inscriptions: HashMap<InscriptionId, ()>,
  outpoint_address: Option<Address>,
}

impl TransactionInputInfo {
  fn extract_from_tx(
    tx: &Transaction,
    index: &Index,
    chain: Chain,
  ) -> Result<HashMap<OutPoint, Self>> {
    let mut res = HashMap::new();
    let rtx = index.rtx()?;
    if tx.is_coin_base() {
      let tx_in = tx.input[0].clone();
      let outpoint_inscriptions =
        index.get_inscriptions_on_output_record(tx_in.previous_output, &rtx)?;
      res.insert(
        tx_in.previous_output,
        TransactionInputInfo {
          tx_in_index: 0,
          witness: tx_in.witness.clone(),
          outpoint_inscriptions,
          outpoint_address: None,
        },
      );
    } else {
      for (tx_in_index, tx_in) in tx.input.iter().enumerate() {
        let outpoint_inscriptions =
          index.get_inscriptions_on_output_record(tx_in.previous_output, &rtx)?;

        let outpoint_address = match index.get_outpoint_address(tx_in.previous_output, chain, &rtx)
        {
          Ok(address) => Some(address),
          Err(err) => {
            log::error!("{}", err);
            None
          }
        };
        res.insert(
          tx_in.previous_output,
          TransactionInputInfo {
            tx_in_index,
            witness: tx_in.witness.clone(),
            outpoint_inscriptions,
            outpoint_address,
          },
        );
      }
    }

    Ok(res)
  }
}
