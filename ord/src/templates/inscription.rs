use super::*;
use crate::templates::transaction::DeserializeExt;
use chrono::LocalResult;
use serde::{
  de::Error as SerdeError,
  ser::{Error, SerializeStruct},
};
use serde_json::Value;

#[derive(Boilerplate)]
pub(crate) struct InscriptionHtml {
  pub(crate) chain: Chain,
  pub(crate) children: Vec<InscriptionId>,
  pub(crate) genesis_fee: u64,
  pub(crate) genesis_height: u64,
  pub(crate) inscription: Inscription,
  pub(crate) inscription_id: InscriptionId,
  pub(crate) next: Option<InscriptionId>,
  pub(crate) inscription_number: i64,
  pub(crate) output: Option<TxOut>,
  pub(crate) parent: Option<InscriptionId>,
  pub(crate) previous: Option<InscriptionId>,
  pub(crate) sat: Option<Sat>,
  pub(crate) satpoint: SatPoint,
  pub(crate) timestamp: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct InscriptionJson {
  pub address: Option<String>,
  pub children: Vec<InscriptionId>,
  pub content_length: Option<usize>,
  pub content_type: Option<String>,
  pub genesis_fee: u64,
  pub genesis_height: u64,
  pub inscription_id: InscriptionId,
  pub next: Option<InscriptionId>,
  pub inscription_number: i64,
  pub output_value: Option<u64>,
  pub parent: Option<InscriptionId>,
  pub previous: Option<InscriptionId>,
  pub sat: Option<Sat>,
  pub satpoint: SatPoint,
  pub timestamp: i64,
}

impl InscriptionJson {
  pub fn new(
    chain: Chain,
    children: Vec<InscriptionId>,
    genesis_fee: u64,
    genesis_height: u64,
    inscription: Inscription,
    inscription_id: InscriptionId,
    parent: Option<InscriptionId>,
    next: Option<InscriptionId>,
    inscription_number: i64,
    output: Option<TxOut>,
    previous: Option<InscriptionId>,
    sat: Option<Sat>,
    satpoint: SatPoint,
    timestamp: DateTime<Utc>,
  ) -> Self {
    Self {
      inscription_id,
      children,
      inscription_number,
      genesis_height,
      parent,
      genesis_fee,
      output_value: output.as_ref().map(|o| o.value),
      address: output
        .as_ref()
        .and_then(|o| chain.address_from_script(&o.script_pubkey).ok())
        .map(|address| address.to_string()),
      sat,
      satpoint,
      content_type: inscription.content_type().map(|s| s.to_string()),
      content_length: inscription.content_length(),
      timestamp: timestamp.timestamp(),
      previous,
      next,
    }
  }
}

impl PageContent for InscriptionHtml {
  fn title(&self) -> String {
    format!("Inscription {}", self.inscription_number)
  }

  fn preview_image_url(&self) -> Option<Trusted<String>> {
    Some(Trusted(format!("/content/{}", self.inscription_id)))
  }
}

#[derive(Serialize, Clone, Deserialize, Debug, PartialEq)]
pub(crate) struct InscriptionRpcContent {
  pub(crate) body: String,
  pub(crate) content_type: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InscriptionOnTx {
  pub(crate) inscription_id: InscriptionId,
  pub(crate) content: InscriptionRpcContent,
  // This displays the index of the inscription in current transaction.
  pub(crate) idx: u32,
  // This displays the index of the txout which contains the inscription of current transaction.
  pub(crate) vout: u32,
  pub(crate) number: i64,
  // This displays the index of the txout in the current transaction to which the inscription has been transferred.
  pub(crate) output_on_tx: OutPoint,
  // This displays the location of the inscription within the current transaction.
  pub(crate) sat_point_on_tx: SatPoint,
  pub(crate) genesis_transaction: Txid,
  pub(crate) from: Option<Address>,
  pub(crate) to: Option<Address>,
}

impl<'de> Deserialize<'de> for InscriptionOnTx {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
    match deserializer.is_human_readable() {
      true => {
        let mut block = serde_json::Value::deserialize(deserializer)?;
        let inscription_id: InscriptionId =
          DeserializeExt::take_from_value::<D>(&mut block, "inscription_id")?;
        let content: InscriptionRpcContent =
          DeserializeExt::take_from_value::<D>(&mut block, "content")?;
        let idx: u32 = DeserializeExt::take_from_value::<D>(&mut block, "idx")?;
        let vout: u32 = DeserializeExt::take_from_value::<D>(&mut block, "vout")?;
        let number: i64 = DeserializeExt::take_from_value::<D>(&mut block, "number")?;
        let output_on_tx: OutPoint =
          DeserializeExt::take_from_value::<D>(&mut block, "output_on_tx")?;
        let sat_point_on_tx: SatPoint =
          DeserializeExt::take_from_value::<D>(&mut block, "sat_point_on_tx")?;
        let genesis_transaction: Txid =
          DeserializeExt::take_from_value::<D>(&mut block, "genesis_transaction")?;
        let from: Option<Address> = {
          let v = block
            .get_mut("from")
            .ok_or_else(|| serde::de::Error::custom(format!("The \"from\" field is missing")))?;
          if v.is_null() {
            None
          } else {
            let address = Address::from_str(v.as_str().unwrap())
              .map_err(|e| serde::de::Error::custom(e.to_string()))?;
            Some(address.assume_checked())
          }
        };
        let to: Option<Address> = {
          let v = block
            .get_mut("to")
            .ok_or_else(|| serde::de::Error::custom(format!("The \"to\" field is missing")))?;
          if v.is_null() {
            None
          } else {
            let address = Address::from_str(v.as_str().unwrap())
              .map_err(|e| serde::de::Error::custom(e.to_string()))?;
            Some(address.assume_checked())
          }
        };
        Ok(InscriptionOnTx {
          inscription_id,
          content,
          idx,
          vout,
          number,
          output_on_tx,
          sat_point_on_tx,
          genesis_transaction,
          from,
          to,
        })
      }
      false => Err(serde::de::Error::custom(
        "Transaction is not human readable",
      )),
    }
  }
}

impl Serialize for InscriptionOnTx {
  fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    match serializer.is_human_readable() {
      true => {
        let mut block = serializer.serialize_struct("InscriptionRaw", 7)?;
        block.serialize_field("inscription_id", &self.inscription_id)?;
        block.serialize_field("content", &self.content)?;
        block.serialize_field("idx", &self.idx)?;
        block.serialize_field("vout", &self.vout)?;
        block.serialize_field("number", &self.number)?;
        block.serialize_field("output_on_tx", &self.output_on_tx)?;
        block.serialize_field("sat_point_on_tx", &self.sat_point_on_tx)?;
        block.serialize_field("genesis_transaction", &self.genesis_transaction)?;

        if let Some(from) = &self.from {
          block.serialize_field("from", &from)?;
        } else {
          block.serialize_field("from", &Value::Null)?;
        }

        if let Some(to) = &self.to {
          block.serialize_field("to", &to)?;
        } else {
          block.serialize_field("to", &Value::Null)?;
        }

        block.end()
      }
      false => Err(S::Error::custom("InscriptionOnTx is not human readable")),
    }
  }
}

#[derive(Clone, Debug)]
pub(crate) struct InscriptionRaw {
  pub(crate) chain: Chain,
  pub(crate) genesis_fee: u64,
  pub(crate) genesis_height: u64,
  pub(crate) genesis_transaction: Txid,
  pub(crate) content: InscriptionRpcContent,
  pub(crate) inscription_id: InscriptionId,
  pub(crate) tx_in_index: u32,
  pub(crate) tx_in_offset: u32,
  pub(crate) next: Option<InscriptionId>,
  pub(crate) number: i64,
  pub(crate) output: Option<TxOut>,
  pub(crate) previous: Option<InscriptionId>,
  pub(crate) sat: Option<Sat>,
  pub(crate) sat_point: SatPoint,
  pub(crate) timestamp: DateTime<Utc>,
}

impl<'de> Deserialize<'de> for InscriptionRaw {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
    match deserializer.is_human_readable() {
      true => {
        let mut block = serde_json::Value::deserialize(deserializer)?;
        let chain: Chain = DeserializeExt::take_from_value::<D>(&mut block, "chain")?;
        let genesis_fee: u64 = DeserializeExt::take_from_value::<D>(&mut block, "genesis_fee")?;
        let genesis_height: u64 =
          DeserializeExt::take_from_value::<D>(&mut block, "genesis_height")?;
        let genesis_transaction: Txid =
          DeserializeExt::take_from_value::<D>(&mut block, "genesis_transaction")?;
        let content: InscriptionRpcContent =
          DeserializeExt::take_from_value::<D>(&mut block, "content")?;
        let inscription_id: InscriptionId =
          DeserializeExt::take_from_value::<D>(&mut block, "inscription_id")?;
        let tx_in_index: u32 = DeserializeExt::take_from_value::<D>(&mut block, "tx_in_index")?;
        let tx_in_offset: u32 = DeserializeExt::take_from_value::<D>(&mut block, "tx_in_offset")?;
        let next: Option<InscriptionId> = DeserializeExt::take_from_value::<D>(&mut block, "next")?;
        let number: i64 = DeserializeExt::take_from_value::<D>(&mut block, "number")?;
        let output: Option<TxOut> = DeserializeExt::take_from_value::<D>(&mut block, "output")?;
        let previous: Option<InscriptionId> =
          DeserializeExt::take_from_value::<D>(&mut block, "previous")?;
        let sat: Option<Sat> = DeserializeExt::take_from_value::<D>(&mut block, "sat")?;
        let sat_point: SatPoint = DeserializeExt::take_from_value::<D>(&mut block, "sat_point")?;
        let time: String = DeserializeExt::take_from_value::<D>(&mut block, "timestamp")?;
        let time = i64::from_str(&time).map_err(|e| SerdeError::custom(e.to_string()))?;
        let timestamp: DateTime<Utc> = match Utc.timestamp_opt(time, 0) {
          LocalResult::None => Err(SerdeError::custom(format!("invalid time {}", time))),
          LocalResult::Single(t) => Ok(t),
          LocalResult::Ambiguous(_, _) => Err(SerdeError::custom(format!("invalid time {}", time))),
        }?;
        Ok(InscriptionRaw {
          chain,
          genesis_fee,
          genesis_height,
          genesis_transaction,
          content,
          inscription_id,
          tx_in_index,
          tx_in_offset,
          next,
          number,
          output,
          previous,
          sat,
          sat_point,
          timestamp,
        })
      }
      false => Err(serde::de::Error::custom(
        "InscriptionRaw is not human readable",
      )),
    }
  }
}

impl Serialize for InscriptionRaw {
  fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    match serializer.is_human_readable() {
      true => {
        let mut block = serializer.serialize_struct("InscriptionRaw", 14)?;
        block.serialize_field("chain", &self.chain)?;
        block.serialize_field("genesis_fee", &self.genesis_fee)?;
        block.serialize_field("genesis_height", &self.genesis_height)?;
        block.serialize_field("genesis_transaction", &self.genesis_transaction)?;
        block.serialize_field("content", &self.content)?;
        block.serialize_field("inscription_id", &self.inscription_id)?;
        block.serialize_field("tx_in_index", &self.tx_in_index)?;
        block.serialize_field("tx_in_offset", &self.tx_in_offset)?;

        if let Some(next) = &self.next {
          block.serialize_field("next", next)?;
        } else {
          block.serialize_field("next", &Value::Null)?;
        }

        block.serialize_field("number", &self.number)?;

        if let Some(output) = &self.output {
          block.serialize_field("output", output)?;
        } else {
          block.serialize_field("output", &Value::Null)?;
        }

        if let Some(previous) = &self.previous {
          block.serialize_field("previous", previous)?;
        } else {
          block.serialize_field("previous", &Value::Null)?;
        }

        if let Some(sat) = &self.sat {
          block.serialize_field("sat", sat)?;
        } else {
          block.serialize_field("sat", &Value::Null)?;
        }

        block.serialize_field("sat_point", &self.sat_point)?;

        block.serialize_field("timestamp", &self.timestamp.timestamp().to_string())?;
        block.end()
      }
      false => Err(S::Error::custom("Inscription is not human readable")),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn without_sat_nav_links_or_output() {
    assert_regex_match!(
      InscriptionHtml {
        children: Vec::new(),
        parent: None,
        chain: Chain::Mainnet,
        genesis_fee: 1,
        genesis_height: 0,
        inscription: inscription("text/plain;charset=utf-8", "HELLOWORLD"),
        inscription_id: inscription_id(1),
        next: None,
        inscription_number: 1,
        output: None,
        previous: None,
        sat: None,
        satpoint: satpoint(1, 0),
        timestamp: timestamp(0),
      },
      "
        <h1>Inscription 1</h1>
        <div class=inscription>
        <div>❮</div>
        <iframe .* src=/preview/1{64}i1></iframe>
        <div>❯</div>
        </div>
        <dl>
          <dt>id</dt>
          <dd class=monospace>1{64}i1</dd>
          <dt>preview</dt>
          <dd><a href=/preview/1{64}i1>link</a></dd>
          <dt>content</dt>
          <dd><a href=/content/1{64}i1>link</a></dd>
          <dt>content length</dt>
          <dd>10 bytes</dd>
          <dt>content type</dt>
          <dd>text/plain;charset=utf-8</dd>
          <dt>timestamp</dt>
          <dd><time>1970-01-01 00:00:00 UTC</time></dd>
          <dt>genesis height</dt>
          <dd><a href=/block/0>0</a></dd>
          <dt>genesis fee</dt>
          <dd>1</dd>
          <dt>genesis transaction</dt>
          <dd><a class=monospace href=/tx/1{64}>1{64}</a></dd>
          <dt>location</dt>
          <dd class=monospace>1{64}:1:0</dd>
          <dt>output</dt>
          <dd><a class=monospace href=/output/1{64}:1>1{64}:1</a></dd>
          <dt>offset</dt>
          <dd>0</dd>
          <dt>ethereum teleburn address</dt>
          <dd>0xa1DfBd1C519B9323FD7Fd8e498Ac16c2E502F059</dd>
        </dl>
      "
      .unindent()
    );
  }

  #[test]
  fn with_output() {
    assert_regex_match!(
      InscriptionHtml {
        children: Vec::new(),
        parent: None,
        chain: Chain::Mainnet,
        genesis_fee: 1,
        genesis_height: 0,
        inscription: inscription("text/plain;charset=utf-8", "HELLOWORLD"),
        inscription_id: inscription_id(1),
        next: None,
        inscription_number: 1,
        output: Some(tx_out(1, address())),
        previous: None,
        sat: None,
        satpoint: satpoint(1, 0),
        timestamp: timestamp(0),
      },
      "
        <h1>Inscription 1</h1>
        <div class=inscription>
        <div>❮</div>
        <iframe .* src=/preview/1{64}i1></iframe>
        <div>❯</div>
        </div>
        <dl>
          .*
          <dt>address</dt>
          <dd class=monospace>bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4</dd>
          <dt>output value</dt>
          <dd>1</dd>
          .*
        </dl>
      "
      .unindent()
    );
  }

  #[test]
  fn with_sat() {
    assert_regex_match!(
      InscriptionHtml {
        children: Vec::new(),
        parent: None,
        chain: Chain::Mainnet,
        genesis_fee: 1,
        genesis_height: 0,
        inscription: inscription("text/plain;charset=utf-8", "HELLOWORLD"),
        inscription_id: inscription_id(1),
        next: None,
        inscription_number: 1,
        output: Some(tx_out(1, address())),
        previous: None,
        sat: Some(Sat(1)),
        satpoint: satpoint(1, 0),
        timestamp: timestamp(0),
      },
      "
        <h1>Inscription 1</h1>
        .*
        <dl>
          .*
          <dt>sat</dt>
          <dd><a href=/sat/1>1</a></dd>
          <dt>preview</dt>
          .*
        </dl>
      "
      .unindent()
    );
  }

  #[test]
  fn with_prev_and_next() {
    assert_regex_match!(
      InscriptionHtml {
        children: Vec::new(),
        parent: None,
        chain: Chain::Mainnet,
        genesis_fee: 1,
        genesis_height: 0,
        inscription: inscription("text/plain;charset=utf-8", "HELLOWORLD"),
        inscription_id: inscription_id(2),
        next: Some(inscription_id(3)),
        inscription_number: 1,
        output: Some(tx_out(1, address())),
        previous: Some(inscription_id(1)),
        sat: None,
        satpoint: satpoint(1, 0),
        timestamp: timestamp(0),
      },
      "
        <h1>Inscription 1</h1>
        <div class=inscription>
        <a class=prev href=/inscription/1{64}i1>❮</a>
        <iframe .* src=/preview/2{64}i2></iframe>
        <a class=next href=/inscription/3{64}i3>❯</a>
        </div>
        .*
      "
      .unindent()
    );
  }

  #[test]
  fn with_cursed_and_unbound() {
    assert_regex_match!(
      InscriptionHtml {
        children: Vec::new(),
        parent: None,
        chain: Chain::Mainnet,
        genesis_fee: 1,
        genesis_height: 0,
        inscription: inscription("text/plain;charset=utf-8", "HELLOWORLD"),
        inscription_id: inscription_id(2),
        next: None,
        inscription_number: -1,
        output: Some(tx_out(1, address())),
        previous: None,
        sat: None,
        satpoint: SatPoint {
          outpoint: unbound_outpoint(),
          offset: 0
        },
        timestamp: timestamp(0),
      },
      "
        <h1>Inscription -1</h1>
        .*
        <dl>
          .*
          <dt>location</dt>
          <dd class=monospace>0{64}:0:0</dd>
          <dt>output</dt>
          <dd><a class=monospace href=/output/0{64}:0>0{64}:0</a></dd>
          .*
        </dl>
      "
      .unindent()
    );
  }

  #[test]
  fn with_parent() {
    assert_regex_match!(
      InscriptionHtml {
        children: Vec::new(),
        parent: Some(inscription_id(2)),
        chain: Chain::Mainnet,
        genesis_fee: 1,
        genesis_height: 0,
        inscription: inscription("text/plain;charset=utf-8", "HELLOWORLD"),
        inscription_id: inscription_id(1),
        next: None,
        inscription_number: 1,
        output: None,
        previous: None,
        sat: None,
        satpoint: satpoint(1, 0),
        timestamp: timestamp(0),
      },
      "
        <h1>Inscription 1</h1>
        <div class=inscription>
        <div>❮</div>
        <iframe .* src=/preview/1{64}i1></iframe>
        <div>❯</div>
        </div>
        <dl>
          <dt>id</dt>
          <dd class=monospace>1{64}i1</dd>
          <dt>parent</dt>
          <dd><a class=monospace href=/inscription/2{64}i2>2{64}i2</a></dd>
          <dt>preview</dt>
          <dd><a href=/preview/1{64}i1>link</a></dd>
          <dt>content</dt>
          <dd><a href=/content/1{64}i1>link</a></dd>
          <dt>content length</dt>
          <dd>10 bytes</dd>
          <dt>content type</dt>
          <dd>text/plain;charset=utf-8</dd>
          <dt>timestamp</dt>
          <dd><time>1970-01-01 00:00:00 UTC</time></dd>
          <dt>genesis height</dt>
          <dd><a href=/block/0>0</a></dd>
          <dt>genesis fee</dt>
          <dd>1</dd>
          <dt>genesis transaction</dt>
          <dd><a class=monospace href=/tx/1{64}>1{64}</a></dd>
          <dt>location</dt>
          <dd class=monospace>1{64}:1:0</dd>
          <dt>output</dt>
          <dd><a class=monospace href=/output/1{64}:1>1{64}:1</a></dd>
          <dt>offset</dt>
          <dd>0</dd>
          <dt>ethereum teleburn address</dt>
          <dd>0xa1DfBd1C519B9323FD7Fd8e498Ac16c2E502F059</dd>
        </dl>
      "
      .unindent()
    );
  }

  #[test]
  fn with_children() {
    assert_regex_match!(
      InscriptionHtml {
        children: vec![inscription_id(2), inscription_id(3)],
        parent: None,
        chain: Chain::Mainnet,
        genesis_fee: 1,
        genesis_height: 0,
        inscription: inscription("text/plain;charset=utf-8", "HELLOWORLD"),
        inscription_id: inscription_id(1),
        next: None,
        inscription_number: 1,
        output: None,
        previous: None,
        sat: None,
        satpoint: satpoint(1, 0),
        timestamp: timestamp(0),
      },
      "
        <h1>Inscription 1</h1>
        <div class=inscription>
        <div>❮</div>
        <iframe .* src=/preview/1{64}i1></iframe>
        <div>❯</div>
        </div>
        <dl>
          <dt>id</dt>
          <dd class=monospace>1{64}i1</dd>
          <dt>preview</dt>
          <dd><a href=/preview/1{64}i1>link</a></dd>
          <dt>content</dt>
          <dd><a href=/content/1{64}i1>link</a></dd>
          <dt>content length</dt>
          <dd>10 bytes</dd>
          <dt>content type</dt>
          <dd>text/plain;charset=utf-8</dd>
          <dt>timestamp</dt>
          <dd><time>1970-01-01 00:00:00 UTC</time></dd>
          <dt>genesis height</dt>
          <dd><a href=/block/0>0</a></dd>
          <dt>genesis fee</dt>
          <dd>1</dd>
          <dt>genesis transaction</dt>
          <dd><a class=monospace href=/tx/1{64}>1{64}</a></dd>
          <dt>location</dt>
          <dd class=monospace>1{64}:1:0</dd>
          <dt>output</dt>
          <dd><a class=monospace href=/output/1{64}:1>1{64}:1</a></dd>
          <dt>offset</dt>
          <dd>0</dd>
          <dt>ethereum teleburn address</dt>
          <dd>0xa1DfBd1C519B9323FD7Fd8e498Ac16c2E502F059</dd>
          <dt>children</dt>
          <dd>
            <div class=thumbnails>
              <a href=/inscription/2{64}i2><iframe .* src=/preview/2{64}i2></iframe></a>
              <a href=/inscription/3{64}i3><iframe .* src=/preview/3{64}i3></iframe></a>
            </div>
          </dd>
        </dl>
      "
      .unindent()
    );
  }
}
