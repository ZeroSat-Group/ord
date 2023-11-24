use super::*;
use serde::de::DeserializeOwned;
use serde::ser::{Error, SerializeStruct};
use serde_json::Value;

#[derive(Boilerplate)]
pub(crate) struct TransactionHtml {
  blockhash: Option<BlockHash>,
  chain: Chain,
  inscription: Option<InscriptionId>,
  transaction: Transaction,
  txid: Txid,
}

impl TransactionHtml {
  pub(crate) fn new(
    transaction: Transaction,
    blockhash: Option<BlockHash>,
    inscription: Option<InscriptionId>,
    chain: Chain,
  ) -> Self {
    Self {
      txid: transaction.txid(),
      blockhash,
      chain,
      inscription,
      transaction,
    }
  }
}

impl PageContent for TransactionHtml {
  fn title(&self) -> String {
    format!("Transaction {}", self.txid)
  }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TransactionRpc {
  pub(crate) chain: Chain,
  pub(crate) height: Option<u64>,
  pub(crate) block_hash: Option<BlockHash>,
  pub(crate) block_time: Option<usize>,
  pub(crate) txid: Txid,
  pub(crate) txidx: Option<u32>,
  pub(crate) time: Option<usize>,
  pub(crate) transaction: Option<Transaction>,
  pub(crate) confirmations: u32,
  pub(crate) inscriptions_ontx: Vec<InscriptionOnTx>,
}

impl<'de> Deserialize<'de> for TransactionRpc {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
    match deserializer.is_human_readable() {
      true => {
        let mut block = serde_json::Value::deserialize(deserializer)?;
        let chain: Chain = DeserializeExt::take_from_value::<D>(&mut block, "chain")?;
        let height: Option<u64> = DeserializeExt::take_from_value::<D>(&mut block, "height")?;
        let block_hash: Option<BlockHash> =
          DeserializeExt::take_from_value::<D>(&mut block, "blockhash")?;
        let block_time: Option<usize> =
          DeserializeExt::take_from_value::<D>(&mut block, "blocktime")?;
        let txid: Txid = DeserializeExt::take_from_value::<D>(&mut block, "txid")?;
        let txidx: Option<u32> = DeserializeExt::take_from_value::<D>(&mut block, "txidx")?;
        let time: Option<usize> = DeserializeExt::take_from_value::<D>(&mut block, "time")?;
        let transaction: Option<Transaction> =
          DeserializeExt::take_from_value::<D>(&mut block, "transaction")?;
        let confirmations: u32 = DeserializeExt::take_from_value::<D>(&mut block, "confirmations")?;
        let inscriptions_ontx: Vec<InscriptionOnTx> =
          DeserializeExt::take_from_value::<D>(&mut block, "inscriptions_ontx")?;
        Ok(TransactionRpc {
          chain,
          height,
          block_hash,
          block_time,
          txid,
          txidx,
          time,
          transaction,
          confirmations,
          inscriptions_ontx,
        })
      }
      false => Err(serde::de::Error::custom(
        "Transaction is not human readable",
      )),
    }
  }
}

impl Serialize for TransactionRpc {
  fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    match serializer.is_human_readable() {
      true => {
        let mut block = serializer.serialize_struct("TransactionRpc", 10)?;
        block.serialize_field("chain", &self.chain)?;
        if let Some(height) = self.height {
          block.serialize_field("height", &height)?;
        } else {
          block.serialize_field("height", &Value::Null)?;
        }

        if let Some(blockhash) = self.block_hash {
          block.serialize_field("blockhash", &blockhash)?;
        } else {
          block.serialize_field("blockhash", &Value::Null)?;
        }

        if let Some(blocktime) = self.block_time {
          block.serialize_field("blocktime", &blocktime)?;
        } else {
          block.serialize_field("blocktime", &Value::Null)?;
        }

        block.serialize_field("txid", &self.txid)?;

        if let Some(txidx) = self.txidx {
          block.serialize_field("txidx", &txidx)?;
        } else {
          block.serialize_field("txidx", &Value::Null)?;
        }

        if let Some(time) = self.time {
          block.serialize_field("time", &time)?;
        } else {
          block.serialize_field("time", &Value::Null)?;
        }

        if let Some(transaction) = &self.transaction {
          block.serialize_field("transaction", transaction)?;
        } else {
          block.serialize_field("transaction", &Value::Null)?;
        }

        block.serialize_field("confirmations", &self.confirmations)?;

        block.serialize_field("inscriptions_ontx", &self.inscriptions_ontx)?;

        block.end()
      }
      false => Err(S::Error::custom("Transaction is not human readable")),
    }
  }
}

#[cfg(test)]
mod tests {
  use {
    super::*,
    bitcoin::{blockdata::script, locktime::absolute::LockTime, TxOut},
  };

  #[test]
  fn html() {
    let transaction = Transaction {
      version: 0,
      lock_time: LockTime::ZERO,
      input: vec![TxIn {
        sequence: Default::default(),
        previous_output: Default::default(),
        script_sig: Default::default(),
        witness: Default::default(),
      }],
      output: vec![
        TxOut {
          value: 50 * COIN_VALUE,
          script_pubkey: script::Builder::new().push_int(0).into_script(),
        },
        TxOut {
          value: 50 * COIN_VALUE,
          script_pubkey: script::Builder::new().push_int(1).into_script(),
        },
      ],
    };

    let txid = transaction.txid();

    pretty_assert_eq!(
      TransactionHtml::new(transaction, None, None, Chain::Mainnet).to_string(),
      format!(
        "
        <h1>Transaction <span class=monospace>{txid}</span></h1>
        <h2>1 Input</h2>
        <ul>
          <li><a class=monospace href=/output/0000000000000000000000000000000000000000000000000000000000000000:4294967295>0000000000000000000000000000000000000000000000000000000000000000:4294967295</a></li>
        </ul>
        <h2>2 Outputs</h2>
        <ul class=monospace>
          <li>
            <a href=/output/{txid}:0 class=monospace>
              {txid}:0
            </a>
            <dl>
              <dt>value</dt><dd>5000000000</dd>
              <dt>script pubkey</dt><dd class=monospace>OP_0</dd>
            </dl>
          </li>
          <li>
            <a href=/output/{txid}:1 class=monospace>
              {txid}:1
            </a>
            <dl>
              <dt>value</dt><dd>5000000000</dd>
              <dt>script pubkey</dt><dd class=monospace>OP_PUSHNUM_1</dd>
            </dl>
          </li>
        </ul>
      "
      )
      .unindent()
    );
  }

  #[test]
  fn with_blockhash() {
    let transaction = Transaction {
      version: 0,
      lock_time: LockTime::ZERO,
      input: Vec::new(),
      output: vec![
        TxOut {
          value: 50 * COIN_VALUE,
          script_pubkey: script::Builder::new().push_int(0).into_script(),
        },
        TxOut {
          value: 50 * COIN_VALUE,
          script_pubkey: script::Builder::new().push_int(1).into_script(),
        },
      ],
    };

    assert_regex_match!(
      TransactionHtml::new(transaction, Some(blockhash(0)), None, Chain::Mainnet),
      "
        <h1>Transaction <span class=monospace>[[:xdigit:]]{64}</span></h1>
        <dl>
          <dt>block</dt>
          <dd><a href=/block/0{64} class=monospace>0{64}</a></dd>
        </dl>
        .*
      "
      .unindent()
    );
  }
}

pub trait DeserializeExt<'de>
where
  Self: DeserializeOwned,
{
  fn take_from_value<D: Deserializer<'de>>(
    value: &mut serde_json::Value,
    field: &str,
  ) -> Result<Self, D::Error>;
}

impl<'de, T> DeserializeExt<'de> for T
where
  T: DeserializeOwned,
{
  fn take_from_value<D: Deserializer<'de>>(
    value: &mut serde_json::Value,
    field: &str,
  ) -> Result<Self, D::Error> {
    serde_json::from_value(
      value
        .get_mut(field)
        .ok_or_else(|| serde::de::Error::custom(format!("The \"{field}\" field is missing")))?
        .take(),
    )
    .map_err(serde::de::Error::custom)
  }
}
