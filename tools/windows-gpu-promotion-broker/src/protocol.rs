use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{COMMAND, PromotionIntent, encode_hex, validate_sha256};

pub const PIPE_ENDPOINT: &str = r"\\.\pipe\ScribeGpuPromotionBroker.v1";
pub const SERVICE_NAME: &str = "ScribeGpuPromotionBroker";
pub const SERVICE_SID: &str = "S-1-5-80-3848011089-2849881844-525567724-3342831801-3217684137";

pub(crate) const FRAME_MAGIC: [u8; 8] = *b"SGPBIPC1";
pub(crate) const PROTOCOL_VERSION: u16 = 1;
pub(crate) const REQUEST_KIND: u16 = 1;
pub(crate) const RESPONSE_KIND: u16 = 2;
pub(crate) const ACK_KIND: u16 = 3;
pub(crate) const FRAME_HEADER_LEN: usize = 16;
pub(crate) const MAX_REQUEST_PAYLOAD: usize = 16 * 1024;
pub(crate) const MAX_RESPONSE_PAYLOAD: usize = 2 * 1024;
pub(crate) const MAX_ACK_PAYLOAD: usize = 1024;
pub(crate) const MAX_REQUEST_FRAME: usize = FRAME_HEADER_LEN + MAX_REQUEST_PAYLOAD;
pub(crate) const MAX_RESPONSE_FRAME: usize = FRAME_HEADER_LEN + MAX_RESPONSE_PAYLOAD;
pub(crate) const MAX_ACK_FRAME: usize = FRAME_HEADER_LEN + MAX_ACK_PAYLOAD;
const REQUEST_DOMAIN: &[u8] = b"scribe-windows-gpu-promotion-request-v1\0";
const RESPONSE_DOMAIN: &[u8] = b"scribe-windows-gpu-promotion-response-v1\0";
const NONCE_HEX_LEN: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerRequestV1 {
    pub schema_version: u16,
    pub command: String,
    pub client_nonce: String,
    pub promotion_intent_sha256: String,
    pub intent: PromotionIntent,
}

impl BrokerRequestV1 {
    pub fn new(intent: PromotionIntent, client_nonce: String) -> Result<Self> {
        let promotion_intent_sha256 = intent.sha256()?;
        let request = Self {
            schema_version: 1,
            command: COMMAND.to_owned(),
            client_nonce,
            promotion_intent_sha256,
            intent,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.command != COMMAND {
            bail!("unsupported broker request contract");
        }
        validate_nonce(&self.client_nonce)?;
        validate_sha256(&self.promotion_intent_sha256)?;
        if self.intent.sha256()? != self.promotion_intent_sha256 {
            bail!("broker request intent identity does not match");
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }

    pub fn sha256(&self) -> Result<String> {
        let mut hasher = Sha256::new();
        hasher.update(REQUEST_DOMAIN);
        hasher.update(self.canonical_json()?);
        Ok(encode_hex(&hasher.finalize()))
    }

    pub(crate) fn from_canonical_json(payload: &[u8]) -> Result<Self> {
        let request: Self = serde_json::from_slice(payload)?;
        request.validate()?;
        if request.canonical_json()? != payload {
            bail!("broker request JSON is noncanonical");
        }
        Ok(request)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotProvisionedCode {
    ProductionAuthorityNotProvisioned,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrokerOutcomeV1 {
    NotProvisioned { code: NotProvisionedCode },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerResponseV1 {
    pub schema_version: u16,
    pub client_nonce: String,
    pub promotion_intent_sha256: String,
    pub request_sha256: String,
    pub outcome: BrokerOutcomeV1,
}

impl BrokerResponseV1 {
    pub fn not_provisioned(request: &BrokerRequestV1) -> Result<Self> {
        let response = Self {
            schema_version: 1,
            client_nonce: request.client_nonce.clone(),
            promotion_intent_sha256: request.promotion_intent_sha256.clone(),
            request_sha256: request.sha256()?,
            outcome: BrokerOutcomeV1::NotProvisioned {
                code: NotProvisionedCode::ProductionAuthorityNotProvisioned,
            },
        };
        response.validate_for(request)?;
        Ok(response)
    }

    pub fn validate_for(&self, request: &BrokerRequestV1) -> Result<()> {
        if self.schema_version != 1 {
            bail!("unsupported broker response contract");
        }
        validate_nonce(&self.client_nonce)?;
        validate_sha256(&self.promotion_intent_sha256)?;
        validate_sha256(&self.request_sha256)?;
        if self.client_nonce != request.client_nonce
            || self.promotion_intent_sha256 != request.promotion_intent_sha256
            || self.request_sha256 != request.sha256()?
        {
            bail!("broker response is not bound to the request");
        }
        if !matches!(
            self.outcome,
            BrokerOutcomeV1::NotProvisioned {
                code: NotProvisionedCode::ProductionAuthorityNotProvisioned
            }
        ) {
            bail!("unsupported broker response outcome");
        }
        Ok(())
    }

    pub fn canonical_json(&self, request: &BrokerRequestV1) -> Result<Vec<u8>> {
        self.validate_for(request)?;
        Ok(serde_json::to_vec(self)?)
    }

    pub fn sha256(&self, request: &BrokerRequestV1) -> Result<String> {
        let mut hasher = Sha256::new();
        hasher.update(RESPONSE_DOMAIN);
        hasher.update(self.canonical_json(request)?);
        Ok(encode_hex(&hasher.finalize()))
    }

    pub(crate) fn from_canonical_json(payload: &[u8], request: &BrokerRequestV1) -> Result<Self> {
        let response: Self = serde_json::from_slice(payload)?;
        response.validate_for(request)?;
        if response.canonical_json(request)? != payload {
            bail!("broker response JSON is noncanonical");
        }
        Ok(response)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerAckV1 {
    pub schema_version: u16,
    pub client_nonce: String,
    pub request_sha256: String,
    pub response_sha256: String,
}

impl BrokerAckV1 {
    pub fn for_response(request: &BrokerRequestV1, response: &BrokerResponseV1) -> Result<Self> {
        let ack = Self {
            schema_version: 1,
            client_nonce: request.client_nonce.clone(),
            request_sha256: request.sha256()?,
            response_sha256: response.sha256(request)?,
        };
        ack.validate_for(request, response)?;
        Ok(ack)
    }

    pub fn validate_for(
        &self,
        request: &BrokerRequestV1,
        response: &BrokerResponseV1,
    ) -> Result<()> {
        if self.schema_version != 1 {
            bail!("unsupported broker acknowledgement contract");
        }
        validate_nonce(&self.client_nonce)?;
        validate_sha256(&self.request_sha256)?;
        validate_sha256(&self.response_sha256)?;
        if self.client_nonce != request.client_nonce
            || self.request_sha256 != request.sha256()?
            || self.response_sha256 != response.sha256(request)?
        {
            bail!("broker acknowledgement is not bound to the response");
        }
        Ok(())
    }

    pub fn canonical_json(
        &self,
        request: &BrokerRequestV1,
        response: &BrokerResponseV1,
    ) -> Result<Vec<u8>> {
        self.validate_for(request, response)?;
        Ok(serde_json::to_vec(self)?)
    }

    pub(crate) fn from_canonical_json(
        payload: &[u8],
        request: &BrokerRequestV1,
        response: &BrokerResponseV1,
    ) -> Result<Self> {
        let ack: Self = serde_json::from_slice(payload)?;
        ack.validate_for(request, response)?;
        if ack.canonical_json(request, response)? != payload {
            bail!("broker acknowledgement JSON is noncanonical");
        }
        Ok(ack)
    }
}

pub(crate) fn encode_request_frame(request: &BrokerRequestV1) -> Result<Vec<u8>> {
    encode_frame(
        REQUEST_KIND,
        &request.canonical_json()?,
        MAX_REQUEST_PAYLOAD,
    )
}

pub(crate) fn decode_request_frame(frame: &[u8]) -> Result<BrokerRequestV1> {
    BrokerRequestV1::from_canonical_json(decode_frame(frame, REQUEST_KIND, MAX_REQUEST_PAYLOAD)?)
}

pub(crate) fn encode_response_frame(
    response: &BrokerResponseV1,
    request: &BrokerRequestV1,
) -> Result<Vec<u8>> {
    encode_frame(
        RESPONSE_KIND,
        &response.canonical_json(request)?,
        MAX_RESPONSE_PAYLOAD,
    )
}

pub(crate) fn decode_response_frame(
    frame: &[u8],
    request: &BrokerRequestV1,
) -> Result<BrokerResponseV1> {
    BrokerResponseV1::from_canonical_json(
        decode_frame(frame, RESPONSE_KIND, MAX_RESPONSE_PAYLOAD)?,
        request,
    )
}

pub(crate) fn encode_ack_frame(
    ack: &BrokerAckV1,
    request: &BrokerRequestV1,
    response: &BrokerResponseV1,
) -> Result<Vec<u8>> {
    encode_frame(
        ACK_KIND,
        &ack.canonical_json(request, response)?,
        MAX_ACK_PAYLOAD,
    )
}

pub(crate) fn decode_ack_frame(
    frame: &[u8],
    request: &BrokerRequestV1,
    response: &BrokerResponseV1,
) -> Result<BrokerAckV1> {
    BrokerAckV1::from_canonical_json(
        decode_frame(frame, ACK_KIND, MAX_ACK_PAYLOAD)?,
        request,
        response,
    )
}

fn encode_frame(kind: u16, payload: &[u8], maximum: usize) -> Result<Vec<u8>> {
    if payload.len() > maximum {
        bail!("broker payload exceeds its fixed bound");
    }
    let payload_len = u32::try_from(payload.len()).map_err(|_| anyhow!("payload is too large"))?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    frame.extend_from_slice(&FRAME_MAGIC);
    frame.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    frame.extend_from_slice(&kind.to_le_bytes());
    frame.extend_from_slice(&payload_len.to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

fn decode_frame(frame: &[u8], expected_kind: u16, maximum: usize) -> Result<&[u8]> {
    if frame.len() < FRAME_HEADER_LEN {
        bail!("broker frame is truncated");
    }
    if frame[..8] != FRAME_MAGIC {
        bail!("broker frame magic does not match");
    }
    let version = u16::from_le_bytes([frame[8], frame[9]]);
    let kind = u16::from_le_bytes([frame[10], frame[11]]);
    let payload_len = u32::from_le_bytes([frame[12], frame[13], frame[14], frame[15]]) as usize;
    if version != PROTOCOL_VERSION || kind != expected_kind {
        bail!("unsupported broker frame contract");
    }
    if payload_len > maximum {
        bail!("broker payload exceeds its fixed bound");
    }
    if frame.len() != FRAME_HEADER_LEN + payload_len {
        bail!("broker frame length does not match");
    }
    Ok(&frame[FRAME_HEADER_LEN..])
}

fn validate_nonce(value: &str) -> Result<()> {
    if value.len() != NONCE_HEX_LEN
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("broker nonce is noncanonical");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent() -> PromotionIntent {
        PromotionIntent {
            schema_version: 1,
            policy_namespace: crate::PROMOTION_POLICY_NAMESPACE.to_owned(),
            source_repository: "owner/repo".to_owned(),
            source_ref: "refs/heads/main".to_owned(),
            source_revision: "a".repeat(40),
            workflow_ref:
                "owner/repo/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main"
                    .to_owned(),
            workflow_source_sha: "a".repeat(40),
            run_id: "123".to_owned(),
            run_attempt: "1".to_owned(),
            artifact_id: "456".to_owned(),
            artifact_digest: "b".repeat(64),
            handoff_sha256: "c".repeat(64),
            release_set_digest: "d".repeat(64),
            toolchain_manifest_sha256: "e".repeat(64),
            pack_version: "0.1.0".to_owned(),
            minimum_security_epoch: 1,
            require_unused_release_set: true,
        }
    }

    fn request() -> BrokerRequestV1 {
        BrokerRequestV1::new(intent(), "1a".repeat(32)).unwrap()
    }

    #[test]
    fn request_and_only_outcome_round_trip_canonically() {
        let request = request();
        let request_frame = encode_request_frame(&request).unwrap();
        assert_eq!(decode_request_frame(&request_frame).unwrap(), request);

        let response = BrokerResponseV1::not_provisioned(&request).unwrap();
        let response_frame = encode_response_frame(&response, &request).unwrap();
        assert_eq!(
            decode_response_frame(&response_frame, &request).unwrap(),
            response
        );
        let ack = BrokerAckV1::for_response(&request, &response).unwrap();
        let ack_frame = encode_ack_frame(&ack, &request, &response).unwrap();
        assert_eq!(
            decode_ack_frame(&ack_frame, &request, &response).unwrap(),
            ack
        );
        assert!(matches!(
            response.outcome,
            BrokerOutcomeV1::NotProvisioned {
                code: NotProvisionedCode::ProductionAuthorityNotProvisioned
            }
        ));
    }

    #[test]
    fn canonical_request_response_and_ack_match_the_powershell_golden_vector() {
        const REQUEST_JSON: &str = r#"{"schema_version":1,"command":"promote-windows-pack-set","client_nonce":"1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a","promotion_intent_sha256":"bf7b4002065dcc87c6d7abd70899c76a23c880c82e1869c4ba2bdbf39dcebe3c","intent":{"schema_version":1,"policy_namespace":"scribe-windows-gpu-production-v1","source_repository":"owner/repo","source_ref":"refs/heads/main","source_revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","workflow_ref":"owner/repo/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main","workflow_source_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","run_id":"123","run_attempt":"1","artifact_id":"456","artifact_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","handoff_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","release_set_digest":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","toolchain_manifest_sha256":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","pack_version":"0.1.0","minimum_security_epoch":1,"require_unused_release_set":true}}"#;
        const REQUEST_SHA256: &str =
            "3925971f64ffaf94450d30373183cf912a01a8948a1a8d892831627329568083";
        const RESPONSE_JSON: &str = r#"{"schema_version":1,"client_nonce":"1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a","promotion_intent_sha256":"bf7b4002065dcc87c6d7abd70899c76a23c880c82e1869c4ba2bdbf39dcebe3c","request_sha256":"3925971f64ffaf94450d30373183cf912a01a8948a1a8d892831627329568083","outcome":{"status":"not_provisioned","code":"production_authority_not_provisioned"}}"#;
        const RESPONSE_SHA256: &str =
            "7d4774c4ad2c0f59d57079e33d3729863a2a679739845f21b4a023207b580143";
        const ACK_JSON: &str = r#"{"schema_version":1,"client_nonce":"1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a","request_sha256":"3925971f64ffaf94450d30373183cf912a01a8948a1a8d892831627329568083","response_sha256":"7d4774c4ad2c0f59d57079e33d3729863a2a679739845f21b4a023207b580143"}"#;

        let request = request();
        assert_eq!(request.canonical_json().unwrap(), REQUEST_JSON.as_bytes());
        assert_eq!(request.sha256().unwrap(), REQUEST_SHA256);
        let response = BrokerResponseV1::not_provisioned(&request).unwrap();
        assert_eq!(
            response.canonical_json(&request).unwrap(),
            RESPONSE_JSON.as_bytes()
        );
        assert_eq!(response.sha256(&request).unwrap(), RESPONSE_SHA256);
        let ack = BrokerAckV1::for_response(&request, &response).unwrap();
        assert_eq!(
            ack.canonical_json(&request, &response).unwrap(),
            ACK_JSON.as_bytes()
        );
    }

    #[test]
    fn invocation_paths_cannot_change_wire_request_bytes() {
        let first =
            crate::ClientInvocation::new(r"C:\hostile\handoff", r"C:\hostile\output", intent())
                .unwrap();
        let second = crate::ClientInvocation::new(
            r"D:\unrelated\intake",
            r"E:\unrelated\publication",
            intent(),
        )
        .unwrap();
        let nonce = "1a".repeat(32);
        let first = BrokerRequestV1::new(first.intent, nonce.clone()).unwrap();
        let second = BrokerRequestV1::new(second.intent, nonce).unwrap();
        assert_eq!(
            encode_request_frame(&first).unwrap(),
            encode_request_frame(&second).unwrap()
        );
        let wire = String::from_utf8(first.canonical_json().unwrap()).unwrap();
        for forbidden in ["hostile", "handoff_root", "output_root", "publication"] {
            assert!(!wire.contains(forbidden));
        }
    }

    #[test]
    fn frames_reject_wrong_magic_version_kind_length_truncation_and_trailing_bytes() {
        let frame = encode_request_frame(&request()).unwrap();
        for mutation in 0..6 {
            let mut changed = frame.clone();
            match mutation {
                0 => changed[0] ^= 1,
                1 => changed[8] = 2,
                2 => changed[10] = RESPONSE_KIND as u8,
                3 => changed[12..16].copy_from_slice(&u32::MAX.to_le_bytes()),
                4 => {
                    changed.pop();
                }
                5 => changed.push(0),
                _ => unreachable!(),
            }
            assert!(
                decode_request_frame(&changed).is_err(),
                "accepted mutation {mutation}"
            );
        }
        assert!(decode_request_frame(&frame[..FRAME_HEADER_LEN - 1]).is_err());
    }

    #[test]
    fn request_rejects_unknown_noncanonical_and_mismatched_identity_fields() {
        let request = request();
        let mut value = serde_json::to_value(&request).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("endpoint".to_owned(), PIPE_ENDPOINT.into());
        assert!(
            BrokerRequestV1::from_canonical_json(&serde_json::to_vec(&value).unwrap()).is_err()
        );

        let canonical = request.canonical_json().unwrap();
        let pretty = serde_json::to_vec_pretty(&request).unwrap();
        assert_ne!(pretty, canonical);
        assert!(BrokerRequestV1::from_canonical_json(&pretty).is_err());

        for mutate in 0..4 {
            let mut changed = request.clone();
            match mutate {
                0 => changed.schema_version = 2,
                1 => changed.command = "other".to_owned(),
                2 => changed.client_nonce = "A".repeat(64),
                3 => changed.promotion_intent_sha256 = "f".repeat(64),
                _ => unreachable!(),
            }
            assert!(changed.validate().is_err());
        }
    }

    #[test]
    fn response_rejects_unknown_noncanonical_and_cross_request_bindings() {
        let request = request();
        let response = BrokerResponseV1::not_provisioned(&request).unwrap();
        let mut value = serde_json::to_value(&response).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("diagnostic".to_owned(), "forbidden".into());
        assert!(
            BrokerResponseV1::from_canonical_json(&serde_json::to_vec(&value).unwrap(), &request)
                .is_err()
        );

        let mut other = request.clone();
        other.client_nonce = "2b".repeat(32);
        assert!(response.validate_for(&other).is_err());
        let pretty = serde_json::to_vec_pretty(&response).unwrap();
        assert!(BrokerResponseV1::from_canonical_json(&pretty, &request).is_err());
    }

    #[test]
    fn acknowledgement_rejects_unknown_noncanonical_and_cross_response_bindings() {
        let request = request();
        let response = BrokerResponseV1::not_provisioned(&request).unwrap();
        let ack = BrokerAckV1::for_response(&request, &response).unwrap();
        let mut value = serde_json::to_value(&ack).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("diagnostic".to_owned(), "forbidden".into());
        assert!(
            BrokerAckV1::from_canonical_json(
                &serde_json::to_vec(&value).unwrap(),
                &request,
                &response,
            )
            .is_err()
        );

        let mut other_request = request.clone();
        other_request.client_nonce = "2b".repeat(32);
        let other_response = BrokerResponseV1::not_provisioned(&other_request).unwrap();
        assert!(ack.validate_for(&other_request, &other_response).is_err());
        let pretty = serde_json::to_vec_pretty(&ack).unwrap();
        assert!(BrokerAckV1::from_canonical_json(&pretty, &request, &response).is_err());
    }

    #[test]
    fn frames_enforce_payload_bounds_before_parsing() {
        let mut request_frame = Vec::with_capacity(FRAME_HEADER_LEN + MAX_REQUEST_PAYLOAD + 1);
        request_frame.extend_from_slice(&FRAME_MAGIC);
        request_frame.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        request_frame.extend_from_slice(&REQUEST_KIND.to_le_bytes());
        request_frame.extend_from_slice(&((MAX_REQUEST_PAYLOAD + 1) as u32).to_le_bytes());
        request_frame.resize(FRAME_HEADER_LEN + MAX_REQUEST_PAYLOAD + 1, b'x');
        assert!(decode_request_frame(&request_frame).is_err());

        let mut ack_frame = Vec::with_capacity(FRAME_HEADER_LEN + MAX_ACK_PAYLOAD + 1);
        ack_frame.extend_from_slice(&FRAME_MAGIC);
        ack_frame.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        ack_frame.extend_from_slice(&ACK_KIND.to_le_bytes());
        ack_frame.extend_from_slice(&((MAX_ACK_PAYLOAD + 1) as u32).to_le_bytes());
        ack_frame.resize(FRAME_HEADER_LEN + MAX_ACK_PAYLOAD + 1, b'x');
        let request = request();
        let response = BrokerResponseV1::not_provisioned(&request).unwrap();
        assert!(decode_ack_frame(&ack_frame, &request, &response).is_err());
    }
}
