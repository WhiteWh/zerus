//! Endpoint primitives; relay credentials never establish cryptographic trust.
//!
//! `open` authenticates against a caller-supplied, already-authorized sender.
//! Membership, expiry, durable request journals and atomic outgoing persistence
//! are mandatory integration responsibilities. HPKE supplies no replay defense.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hmac::{Hmac, Mac};
use hpke::{
    aead::AesGcm256, kdf::HkdfSha256, kem::X25519HkdfSha256, Deserializable, Kem, OpModeR, OpModeS,
    Serializable,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

type K = X25519HkdfSha256;
type HmacSha256 = Hmac<Sha256>;
pub const VERSION: u8 = 1;
pub const MAX_PAYLOAD_BYTES: usize = 29 * 1024 * 1024;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_INVITATION_CODE_BYTES: usize = 4096;
pub const MAX_MEMBERSHIP_DEVICES: usize = 4096;
pub const MAX_WIRE_INTEGER: u64 = i64::MAX as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Authentication,
    Unsupported,
    TooLarge,
    Randomness,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "encrypted transport {:?}", self)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// A typed, unambiguous byte frame shared with native clients. The domain is
/// ASCII followed by NUL. Every subsequent field is u32BE length + exact bytes.
pub fn frame(domain: &str, fields: &[&[u8]]) -> Result<Vec<u8>> {
    if domain.len() > 128 || !domain.is_ascii() || domain.as_bytes().contains(&0) {
        return Err(Error::Invalid);
    }
    let mut output = Vec::new();
    output.extend_from_slice(domain.as_bytes());
    output.push(0);
    for field in fields {
        let length = u32::try_from(field.len()).map_err(|_| Error::TooLarge)?;
        output.extend_from_slice(&length.to_be_bytes());
        output.extend_from_slice(field);
    }
    Ok(output)
}
pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}
pub fn decode(value: &str, max: usize) -> Result<Vec<u8>> {
    if value.len() > max.saturating_add(2).saturating_mul(4) / 3 {
        return Err(Error::TooLarge);
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| Error::Invalid)?;
    // URL_SAFE_NO_PAD requires absent padding and rejects nonzero unused bits;
    // re-encoding to check canonicality would allocate another payload-sized
    // string. Keep the engine's strict canonical decoder instead.
    if bytes.len() > max {
        return Err(Error::Invalid);
    }
    Ok(bytes)
}
pub fn decode_fixed<const N: usize>(value: &str) -> Result<[u8; N]> {
    decode(value, N)?.try_into().map_err(|_| Error::Invalid)
}
fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn verify(key: &str, input: &[u8], signature: &str) -> Result<()> {
    let key = validated_signing_key(key)?;
    let signature = Signature::from_bytes(&decode_fixed(signature)?);
    key.verify_strict(input, &signature)
        .map_err(|_| Error::Authentication)
}
fn validated_signing_key(value: &str) -> Result<VerifyingKey> {
    let bytes = decode_fixed::<32>(value)?;
    let mut y = bytes;
    y[31] &= 0x7f;
    let mut p = [0xff; 32];
    p[0] = 0xed;
    p[31] = 0x7f;
    if y.iter().rev().cmp(p.iter().rev()) != std::cmp::Ordering::Less {
        return Err(Error::Invalid);
    }
    let key = VerifyingKey::from_bytes(&bytes).map_err(|_| Error::Invalid)?;
    if key.is_weak() {
        return Err(Error::Invalid);
    }
    Ok(key)
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Computer,
    Phone,
}
impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Computer => "computer",
            Self::Phone => "phone",
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DevicePublic {
    pub device_id: String,
    pub role: Role,
    pub key_generation: u64,
    pub signing_public_key: String,
    pub encryption_public_key: String,
}
impl DevicePublic {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.device_id)?;
        if self.key_generation == 0 || self.key_generation > MAX_WIRE_INTEGER {
            return Err(Error::Invalid);
        }
        validated_signing_key(&self.signing_public_key)?;
        let kem = decode_fixed::<32>(&self.encryption_public_key)?;
        // X25519 rejects low-order points during DH. Reject the ubiquitous zero
        // key before it can become an admitted identity as well.
        if kem == [0; 32] {
            return Err(Error::Invalid);
        }
        <K as Kem>::PublicKey::from_bytes(&kem).map_err(|_| Error::Invalid)?;
        Ok(())
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        frame(
            "zerus-e2ee-device-v1",
            &[
                self.device_id.as_bytes(),
                self.role.name().as_bytes(),
                &self.key_generation.to_be_bytes(),
                &decode_fixed::<32>(&self.signing_public_key)?,
                &decode_fixed::<32>(&self.encryption_public_key)?,
            ],
        )
    }
}

/// Private local state. Deliberately has no Debug/Display implementation.
/// Serialize only into the platform's protected private storage, never a relay
/// request, crash report, environment variable, invitation or project file.
pub struct Identity {
    device: DevicePublic,
    signing: SigningKey,
    encryption: <K as Kem>::PrivateKey,
}
/// Storage representation, also deliberately not Debug. Strings are wiped on
/// drop; serializers and storage adapters must protect their own temporary copy.
#[derive(Deserialize, Serialize, Zeroize)]
#[serde(deny_unknown_fields)]
#[zeroize(drop)]
pub struct IdentityRecord {
    #[zeroize(skip)]
    pub version: u8,
    #[zeroize(skip)]
    pub device: DevicePublic,
    pub signing_private_key: String,
    pub encryption_private_key: String,
}
impl Identity {
    pub fn generate(device_id: String, role: Role) -> Result<Self> {
        let mut signing_seed = Zeroizing::new([0u8; 32]);
        let mut kem_seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *signing_seed).map_err(|_| Error::Randomness)?;
        getrandom::fill(&mut *kem_seed).map_err(|_| Error::Randomness)?;
        let signing = SigningKey::from_bytes(&signing_seed);
        let (encryption, public) = K::derive_keypair(&*kem_seed);
        let device = DevicePublic {
            device_id,
            role,
            key_generation: 1,
            signing_public_key: encode(&signing.verifying_key().to_bytes()),
            encryption_public_key: encode(&public.to_bytes()),
        };
        device.validate()?;
        Ok(Self {
            device,
            signing,
            encryption,
        })
    }
    pub fn from_record(record: &IdentityRecord) -> Result<Self> {
        if record.version != VERSION {
            return Err(Error::Unsupported);
        }
        record.device.validate()?;
        let signing_seed = Zeroizing::new(decode_fixed::<32>(&record.signing_private_key)?);
        let encryption_seed = Zeroizing::new(decode_fixed::<32>(&record.encryption_private_key)?);
        let signing = SigningKey::from_bytes(&signing_seed);
        let encryption =
            <K as Kem>::PrivateKey::from_bytes(&*encryption_seed).map_err(|_| Error::Invalid)?;
        if encode(&signing.verifying_key().to_bytes()) != record.device.signing_public_key
            || encode(&K::sk_to_pk(&encryption).to_bytes()) != record.device.encryption_public_key
        {
            return Err(Error::Authentication);
        }
        Ok(Self {
            device: record.device.clone(),
            signing,
            encryption,
        })
    }
    pub fn to_record(&self) -> IdentityRecord {
        IdentityRecord {
            version: VERSION,
            device: self.device.clone(),
            signing_private_key: encode(&self.signing.to_bytes()),
            encryption_private_key: encode(&self.encryption.to_bytes()),
        }
    }
    pub fn public(&self) -> &DevicePublic {
        &self.device
    }
    pub fn sign(&self, input: &[u8]) -> String {
        encode(&self.signing.sign(input).to_bytes())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PayloadKind {
    Request,
    Result,
    Snapshot,
    Membership,
}
impl PayloadKind {
    fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Result => "result",
            Self::Snapshot => "snapshot",
            Self::Membership => "membership",
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OperationClass {
    Read,
    Mutation,
    Control,
}
impl OperationClass {
    fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Mutation => "mutation",
            Self::Control => "control",
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub workspace_id: String,
    pub sender_device_id: String,
    pub recipient_device_id: String,
    pub sender_key_generation: u64,
    pub recipient_key_generation: u64,
    pub gateway_id: String,
    pub route_id: String,
    pub request_id: String,
    pub kind: PayloadKind,
    pub operation_class: OperationClass,
    pub expires_at: u64,
    pub membership_revision: u64,
    pub membership_head: String,
}
impl Header {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        for id in [
            &self.workspace_id,
            &self.sender_device_id,
            &self.recipient_device_id,
            &self.gateway_id,
            &self.route_id,
            &self.request_id,
        ] {
            identifier(id)?;
        }
        if self.sender_key_generation == 0
            || self.recipient_key_generation == 0
            || self.expires_at == 0
            || [
                self.sender_key_generation,
                self.recipient_key_generation,
                self.expires_at,
                self.membership_revision,
            ]
            .iter()
            .any(|n| *n > MAX_WIRE_INTEGER)
        {
            return Err(Error::Invalid);
        }
        frame(
            "zerus-e2ee-header-v1",
            &[
                self.workspace_id.as_bytes(),
                self.sender_device_id.as_bytes(),
                self.recipient_device_id.as_bytes(),
                &self.sender_key_generation.to_be_bytes(),
                &self.recipient_key_generation.to_be_bytes(),
                self.gateway_id.as_bytes(),
                self.route_id.as_bytes(),
                self.request_id.as_bytes(),
                self.kind.name().as_bytes(),
                self.operation_class.name().as_bytes(),
                &self.expires_at.to_be_bytes(),
                &self.membership_revision.to_be_bytes(),
                &decode_fixed::<32>(&self.membership_head)?,
            ],
        )
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EncryptedEnvelope {
    pub version: u8,
    pub header: Header,
    pub enc: String,
    pub ciphertext: String,
    pub signature: String,
}
impl EncryptedEnvelope {
    fn signing_length(&self) -> Result<usize> {
        if self.version != VERSION {
            return Err(Error::Unsupported);
        }
        if self.ciphertext.len() > (MAX_PAYLOAD_BYTES + 18) * 4 / 3
            || self.ciphertext.len() % 4 == 1
        {
            return Err(Error::TooLarge);
        }
        let length = self.ciphertext.len() * 3 / 4;
        if !(16..=MAX_PAYLOAD_BYTES + 16).contains(&length) {
            return Err(Error::Invalid);
        }
        Ok(b"zerus-e2ee-envelope-v1\0".len()
            + 12
            + self.header.canonical_bytes()?.len()
            + 32
            + length)
    }
    /// Feed the frozen standard Ed25519 message without allocating either the
    /// full decoded ciphertext or another full signature frame. Chunk sizes
    /// are multiples of a base64 quantum; only the final chunk may be partial.
    fn signing_chunks(&self, mut consume: impl FnMut(&[u8])) -> Result<()> {
        self.signing_length()?;
        let header = self.header.canonical_bytes()?;
        let enc = decode_fixed::<32>(&self.enc)?;
        consume(b"zerus-e2ee-envelope-v1\0");
        consume(&(header.len() as u32).to_be_bytes());
        consume(&header);
        consume(&32u32.to_be_bytes());
        consume(&enc);
        consume(&((self.ciphertext.len() * 3 / 4) as u32).to_be_bytes());
        const CHUNK: usize = 64 * 1024;
        let mut decoded = [0u8; CHUNK / 4 * 3];
        for chunk in self.ciphertext.as_bytes().chunks(CHUNK) {
            let length = URL_SAFE_NO_PAD
                .decode_slice(chunk, &mut decoded)
                .map_err(|_| Error::Invalid)?;
            consume(&decoded[..length]);
        }
        Ok(())
    }
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        let mut result = Vec::with_capacity(self.signing_length()?);
        self.signing_chunks(|chunk| result.extend_from_slice(chunk))?;
        Ok(result)
    }
    /// Verify without decrypting. Caller must provide a member from current,
    /// endpoint-owned authorization state, never from this envelope or relay.
    pub fn verify_sender(&self, authorized_sender: &DevicePublic) -> Result<()> {
        authorized_sender.validate()?;
        if self.header.sender_device_id != authorized_sender.device_id
            || self.header.sender_key_generation != authorized_sender.key_generation
        {
            return Err(Error::Authentication);
        }
        let key = validated_signing_key(&authorized_sender.signing_public_key)?;
        let signature_bytes = decode_fixed::<64>(&self.signature)?;
        // Dalek's stream verifier validates canonical S and recomputes canonical
        // R, but does not perform verify_strict's small-order R rejection.
        // Use Dalek's public point decoder and weak-point predicate for R too;
        // no scalar arithmetic or signature algorithm is implemented here.
        validated_signing_key(&encode(&signature_bytes[..32]))?;
        let signature = Signature::from_bytes(&signature_bytes);
        let mut verifier = key
            .verify_stream(&signature)
            .map_err(|_| Error::Authentication)?;
        self.signing_chunks(|chunk| verifier.update(chunk))?;
        verifier
            .finalize_and_verify()
            .map_err(|_| Error::Authentication)
    }
    /// Digest of exact signed semantic envelope, for immutable ciphertext outbox
    /// retries and durable replay/conflict checks. Re-encryption is not a retry.
    pub fn digest(&self) -> Result<String> {
        let length = u32::try_from(self.signing_length()?).map_err(|_| Error::TooLarge)?;
        let signature = decode_fixed::<64>(&self.signature)?;
        let mut hash = Sha256::new();
        hash.update(b"zerus-e2ee-envelope-digest-v1\0");
        hash.update(length.to_be_bytes());
        self.signing_chunks(|chunk| hash.update(chunk))?;
        hash.update(64u32.to_be_bytes());
        hash.update(signature);
        Ok(encode(&hash.finalize()))
    }
}
pub fn seal(
    identity: &Identity,
    recipient: &DevicePublic,
    header: Header,
    plaintext: &[u8],
) -> Result<EncryptedEnvelope> {
    if plaintext.len() > MAX_PAYLOAD_BYTES {
        return Err(Error::TooLarge);
    }
    recipient.validate()?;
    if header.sender_device_id != identity.device.device_id
        || header.sender_key_generation != identity.device.key_generation
        || header.recipient_device_id != recipient.device_id
        || header.recipient_key_generation != recipient.key_generation
    {
        return Err(Error::Authentication);
    }
    let public =
        <K as Kem>::PublicKey::from_bytes(&decode_fixed::<32>(&recipient.encryption_public_key)?)
            .map_err(|_| Error::Invalid)?;
    let (enc, mut context) = hpke::setup_sender::<AesGcm256, HkdfSha256, K>(
        &OpModeS::Base,
        &public,
        &header.canonical_bytes()?,
    )
    .map_err(|_| Error::Authentication)?;
    let ciphertext = context
        .seal(plaintext, b"")
        .map_err(|_| Error::Authentication)?;
    let mut envelope = EncryptedEnvelope {
        version: VERSION,
        header,
        enc: encode(&enc.to_bytes()),
        ciphertext: encode(&ciphertext),
        signature: String::new(),
    };
    envelope.signature = identity.sign(&envelope.signing_bytes()?);
    Ok(envelope)
}
pub fn open(
    identity: &Identity,
    authorized_sender: &DevicePublic,
    expected_header: &Header,
    envelope: &EncryptedEnvelope,
) -> Result<Zeroizing<Vec<u8>>> {
    // Exact caller expectation prevents cross-workspace, route, direction and
    // request substitution. Signature is checked before any plaintext escapes.
    if &envelope.header != expected_header
        || envelope.header.recipient_device_id != identity.device.device_id
        || envelope.header.recipient_key_generation != identity.device.key_generation
    {
        return Err(Error::Authentication);
    }
    envelope.verify_sender(authorized_sender)?;
    let enc = <K as Kem>::EncappedKey::from_bytes(&decode_fixed::<32>(&envelope.enc)?)
        .map_err(|_| Error::Invalid)?;
    let mut context = hpke::setup_receiver::<AesGcm256, HkdfSha256, K>(
        &OpModeR::Base,
        &identity.encryption,
        &enc,
        &envelope.header.canonical_bytes()?,
    )
    .map_err(|_| Error::Authentication)?;
    let plaintext = context
        .open(&decode(&envelope.ciphertext, MAX_PAYLOAD_BYTES + 16)?, b"")
        .map_err(|_| Error::Authentication)?;
    Ok(Zeroizing::new(plaintext))
}

/// Copied invitation. The secret never belongs in an HTTP request. Code Debug
/// is intentionally absent. The outer URL parser owns validated HTTPS origin.
#[derive(Clone, Deserialize, Serialize, Zeroize)]
#[serde(deny_unknown_fields)]
#[zeroize(drop)]
pub struct Invitation {
    #[zeroize(skip)]
    pub version: u8,
    pub workspace_id: String,
    pub invitation_id: String,
    #[zeroize(skip)]
    pub purpose: Role,
    #[zeroize(skip)]
    pub issuer: DevicePublic,
    #[zeroize(skip)]
    pub expires_at: u64,
    #[zeroize(skip)]
    pub membership_revision: u64,
    pub membership_head: String,
    pub secret: String,
}
impl Invitation {
    pub fn generate(
        workspace_id: String,
        invitation_id: String,
        purpose: Role,
        issuer: DevicePublic,
        expires_at: u64,
        membership_revision: u64,
        membership_head: String,
    ) -> Result<Self> {
        let mut secret = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *secret).map_err(|_| Error::Randomness)?;
        let result = Self {
            version: VERSION,
            workspace_id,
            invitation_id,
            purpose,
            issuer,
            expires_at,
            membership_revision,
            membership_head,
            secret: encode(&*secret),
        };
        result.validate()?;
        Ok(result)
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::Unsupported);
        }
        identifier(&self.workspace_id)?;
        identifier(&self.invitation_id)?;
        self.issuer.validate()?;
        if self.issuer.role != Role::Computer
            || self.expires_at == 0
            || self.expires_at > MAX_WIRE_INTEGER
            || self.membership_revision > MAX_WIRE_INTEGER
        {
            return Err(Error::Invalid);
        }
        decode_fixed::<32>(&self.membership_head)?;
        decode_fixed::<32>(&self.secret)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MembershipAction {
    Add,
    Revoke,
}
impl MembershipAction {
    fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Revoke => "revoke",
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MembershipStatement {
    pub workspace_id: String,
    pub revision: u64,
    pub previous_head: String,
    pub issuer_device_id: String,
    pub issuer_key_generation: u64,
    pub action: MembershipAction,
    pub device: Option<DevicePublic>,
    pub revoked_device_id: Option<String>,
}
impl MembershipStatement {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        identifier(&self.workspace_id)?;
        identifier(&self.issuer_device_id)?;
        if self.revision > MAX_WIRE_INTEGER
            || self.issuer_key_generation == 0
            || self.issuer_key_generation > MAX_WIRE_INTEGER
        {
            return Err(Error::Invalid);
        }
        match self.action {
            MembershipAction::Add if self.device.is_some() && self.revoked_device_id.is_none() => {}
            MembershipAction::Revoke
                if self.device.is_none() && self.revoked_device_id.is_some() =>
            {
                identifier(self.revoked_device_id.as_ref().ok_or(Error::Invalid)?)?
            }
            _ => return Err(Error::Invalid),
        }
        let device = self
            .device
            .as_ref()
            .map(DevicePublic::canonical_bytes)
            .transpose()?
            .unwrap_or_default();
        frame(
            "zerus-e2ee-membership-v1",
            &[
                self.workspace_id.as_bytes(),
                &self.revision.to_be_bytes(),
                &decode_fixed::<32>(&self.previous_head)?,
                self.issuer_device_id.as_bytes(),
                &self.issuer_key_generation.to_be_bytes(),
                self.action.name().as_bytes(),
                &device,
                self.revoked_device_id.as_deref().unwrap_or("").as_bytes(),
            ],
        )
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedMembership {
    pub version: u8,
    pub statement: MembershipStatement,
    pub signature: String,
}
impl SignedMembership {
    pub fn sign(statement: MembershipStatement, identity: &Identity) -> Result<Self> {
        if statement.issuer_device_id != identity.device.device_id
            || statement.issuer_key_generation != identity.device.key_generation
            || identity.device.role != Role::Computer
        {
            return Err(Error::Authentication);
        }
        let signature = identity.sign(&statement.canonical_bytes()?);
        Ok(Self {
            version: VERSION,
            statement,
            signature,
        })
    }
    pub fn verify_signature(&self, issuer: &DevicePublic) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::Unsupported);
        }
        issuer.validate()?;
        if issuer.role != Role::Computer
            || issuer.device_id != self.statement.issuer_device_id
            || issuer.key_generation != self.statement.issuer_key_generation
        {
            return Err(Error::Authentication);
        }
        verify(
            &issuer.signing_public_key,
            &self.statement.canonical_bytes()?,
            &self.signature,
        )
    }
    pub fn verify_genesis(&self) -> Result<()> {
        let s = &self.statement;
        let founder = s.device.as_ref().ok_or(Error::Invalid)?;
        if s.revision != 0
            || decode_fixed::<32>(&s.previous_head)? != [0; 32]
            || s.action != MembershipAction::Add
            || founder.role != Role::Computer
            || founder.device_id != s.issuer_device_id
        {
            return Err(Error::Authentication);
        }
        self.verify_signature(founder)
    }
    /// Checks the exact preceding durable head and current issuer. Membership
    /// mutation policy is enforced by MembershipState::apply_next.
    pub fn verify_successor(
        &self,
        workspace_id: &str,
        previous_revision: u64,
        previous_head: &str,
        active_issuer: &DevicePublic,
    ) -> Result<()> {
        if self.statement.workspace_id != workspace_id
            || self.statement.revision != previous_revision.checked_add(1).ok_or(Error::Invalid)?
            || self.statement.previous_head != previous_head
        {
            return Err(Error::Authentication);
        }
        self.verify_signature(active_issuer)
    }
    pub fn head(&self) -> Result<String> {
        if self.version != VERSION {
            return Err(Error::Unsupported);
        }
        Ok(encode(&Sha256::digest(frame(
            "zerus-e2ee-membership-head-v1",
            &[
                &self.statement.canonical_bytes()?,
                &decode_fixed::<64>(&self.signature)?,
            ],
        )?)))
    }
}

/// The domain of the person's latch statement (zerus-sa9qn.50).
pub const ROUTE_PERSON_DOMAIN: &str = "zerus-question-route-person-v1";
/// The longest a latch statement stays valid, in seconds.
pub const ROUTE_PERSON_LIFETIME: u64 = 300;
/// How far a phone's clock may run ahead of the gateway's, in seconds.
pub const ROUTE_PERSON_SKEW: u64 = 60;

/// The person's latch on one child's question route (zerus-sa9qn.50): a
/// phone of the workspace signs it for one gateway, whose desktop service
/// applies it at most once (by `request_id`) before `expires_at`. It sets
/// `person_off` to its value; it is not a compare-and-swap.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoutePersonStatement {
    pub workspace_id: String,
    pub device_id: String,
    pub gateway_id: String,
    pub session_id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub person_off: bool,
    pub request_id: String,
    pub issued_at: u64,
    pub expires_at: u64,
}
impl RoutePersonStatement {
    pub fn validate(&self) -> Result<()> {
        for id in [
            &self.workspace_id,
            &self.device_id,
            &self.gateway_id,
            &self.session_id,
            &self.request_id,
        ] {
            identifier(id)?;
        }
        for (value, maximum) in [(&self.run_id, 128), (&self.conversation_id, 256)] {
            if value.is_empty()
                || value.len() > maximum
                || value.chars().any(|c| c < ' ' || c == '\u{7f}')
            {
                return Err(Error::Invalid);
            }
        }
        if self.issued_at >= self.expires_at
            || self.expires_at - self.issued_at > ROUTE_PERSON_LIFETIME
            || self.expires_at > MAX_WIRE_INTEGER
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        frame(
            ROUTE_PERSON_DOMAIN,
            &[
                self.workspace_id.as_bytes(),
                self.device_id.as_bytes(),
                self.gateway_id.as_bytes(),
                self.session_id.as_bytes(),
                self.run_id.as_bytes(),
                self.conversation_id.as_bytes(),
                &[u8::from(self.person_off)],
                self.request_id.as_bytes(),
                &self.issued_at.to_be_bytes(),
                &self.expires_at.to_be_bytes(),
            ],
        )
    }
    pub fn sign(&self, identity: &Identity) -> Result<String> {
        Ok(identity.sign(&self.canonical_bytes()?))
    }
    /// A current phone member of exactly this workspace signed it, and it is
    /// valid at `now` (seconds). The gateway checks `gateway_id` itself.
    pub fn verify(&self, membership: &MembershipState, signature: &str, now: u64) -> Result<()> {
        membership.validate()?;
        let signer = membership
            .members
            .get(&self.device_id)
            .ok_or(Error::Authentication)?;
        if membership.workspace_id != self.workspace_id
            || signer.role != Role::Phone
            || now.saturating_add(ROUTE_PERSON_SKEW) < self.issued_at
            || now >= self.expires_at
        {
            return Err(Error::Authentication);
        }
        verify(
            &signer.signing_public_key,
            &self.canonical_bytes()?,
            signature,
        )
    }
}

/// Linear, endpoint-owned membership state. Persist the state and consumed
/// history row in one transaction before acknowledging or using the new head.
/// A relay snapshot may not replace this pinned state.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MembershipState {
    pub workspace_id: String,
    pub revision: u64,
    pub head: String,
    pub members: BTreeMap<String, DevicePublic>,
    pub revoked_device_ids: BTreeSet<String>,
}
impl MembershipState {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.workspace_id)?;
        if self.revision > MAX_WIRE_INTEGER
            || self.members.len() + self.revoked_device_ids.len() > MAX_MEMBERSHIP_DEVICES
        {
            return Err(Error::TooLarge);
        }
        decode_fixed::<32>(&self.head)?;
        for (id, member) in &self.members {
            member.validate()?;
            if id != &member.device_id || self.revoked_device_ids.contains(id) {
                return Err(Error::Authentication);
            }
        }
        for id in &self.revoked_device_ids {
            identifier(id)?;
        }
        Ok(())
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut fields = vec![
            self.workspace_id.as_bytes().to_vec(),
            self.revision.to_be_bytes().to_vec(),
            decode_fixed::<32>(&self.head)?.to_vec(),
            (self.members.len() as u64).to_be_bytes().to_vec(),
        ];
        for member in self.members.values() {
            fields.push(member.canonical_bytes()?);
        }
        fields.push(
            (self.revoked_device_ids.len() as u64)
                .to_be_bytes()
                .to_vec(),
        );
        for id in &self.revoked_device_ids {
            fields.push(id.as_bytes().to_vec());
        }
        frame(
            "zerus-e2ee-membership-state-v1",
            &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )
    }
    pub fn from_genesis(genesis: &SignedMembership) -> Result<Self> {
        genesis.verify_genesis()?;
        let device = genesis
            .statement
            .device
            .as_ref()
            .ok_or(Error::Invalid)?
            .clone();
        Ok(Self {
            workspace_id: genesis.statement.workspace_id.clone(),
            revision: 0,
            head: genesis.head()?,
            members: BTreeMap::from([(device.device_id.clone(), device)]),
            revoked_device_ids: BTreeSet::new(),
        })
    }
    pub fn apply_next(&mut self, change: &SignedMembership) -> Result<()> {
        self.validate()?;
        let s = &change.statement;
        let issuer = self
            .members
            .get(&s.issuer_device_id)
            .ok_or(Error::Authentication)?;
        change.verify_successor(&self.workspace_id, self.revision, &self.head, issuer)?;
        let next_head = change.head()?;
        match s.action {
            MembershipAction::Add => {
                let device = s.device.as_ref().ok_or(Error::Invalid)?;
                if self.members.contains_key(&device.device_id)
                    || self.revoked_device_ids.contains(&device.device_id)
                {
                    return Err(Error::Authentication);
                }
                if self.members.len() + self.revoked_device_ids.len() >= MAX_MEMBERSHIP_DEVICES {
                    return Err(Error::TooLarge);
                }
                self.members
                    .insert(device.device_id.clone(), device.clone());
            }
            MembershipAction::Revoke => {
                let id = s.revoked_device_id.as_ref().ok_or(Error::Invalid)?;
                self.members.get(id).ok_or(Error::Authentication)?;
                self.members.remove(id);
                self.revoked_device_ids.insert(id.clone());
            }
        }
        self.revision = s.revision;
        self.head = next_head;
        Ok(())
    }
    pub fn authorized_sender(&self, envelope: &EncryptedEnvelope) -> Result<&DevicePublic> {
        self.authorized_sender_at_known_head(envelope, &self.head)
    }
    /// `known_head` must come from this endpoint's verified durable history at
    /// envelope.header.membership_revision. This permits delayed ciphertext
    /// across unrelated roster changes, while rejecting removed senders.
    pub fn authorized_sender_at_known_head(
        &self,
        envelope: &EncryptedEnvelope,
        known_head: &str,
    ) -> Result<&DevicePublic> {
        let h = &envelope.header;
        if h.workspace_id != self.workspace_id
            || h.membership_revision > self.revision
            || h.membership_head != known_head
            || (h.membership_revision == self.revision && h.membership_head != self.head)
        {
            return Err(Error::Authentication);
        }
        let member = self
            .members
            .get(&h.sender_device_id)
            .ok_or(Error::Authentication)?;
        let recipient = self
            .members
            .get(&h.recipient_device_id)
            .ok_or(Error::Authentication)?;
        if recipient.key_generation != h.recipient_key_generation {
            return Err(Error::Authentication);
        }
        envelope.verify_sender(member)?;
        Ok(member)
    }
}

/// Bootstrap only: an existing endpoint must synchronize signed history from
/// its pinned head and must never replace state with this snapshot.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedMembershipSnapshot {
    pub version: u8,
    pub state: MembershipState,
    pub issuer_device_id: String,
    pub issuer_key_generation: u64,
    pub signature: String,
}
impl SignedMembershipSnapshot {
    /// Signature and signer binding only; this does not authorize replacing an
    /// existing endpoint's pinned state. The relay may use it for admission.
    pub fn verify_signer(&self, issuer: &DevicePublic) -> Result<()> {
        issuer.validate()?;
        if issuer.role != Role::Computer
            || self.issuer_device_id != issuer.device_id
            || self.issuer_key_generation != issuer.key_generation
            || self.state.members.get(&self.issuer_device_id) != Some(issuer)
        {
            return Err(Error::Authentication);
        }
        verify(
            &issuer.signing_public_key,
            &self.signing_bytes()?,
            &self.signature,
        )
    }
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        if self.version != VERSION {
            return Err(Error::Unsupported);
        }
        identifier(&self.issuer_device_id)?;
        if self.issuer_key_generation == 0 || self.issuer_key_generation > MAX_WIRE_INTEGER {
            return Err(Error::Invalid);
        }
        frame(
            "zerus-e2ee-membership-snapshot-v1",
            &[
                &self.state.canonical_bytes()?,
                self.issuer_device_id.as_bytes(),
                &self.issuer_key_generation.to_be_bytes(),
            ],
        )
    }
    pub fn sign(state: MembershipState, issuer: &Identity) -> Result<Self> {
        state.validate()?;
        if state.members.get(&issuer.device.device_id) != Some(issuer.public())
            || issuer.device.role != Role::Computer
        {
            return Err(Error::Authentication);
        }
        let mut result = Self {
            version: VERSION,
            state,
            issuer_device_id: issuer.device.device_id.clone(),
            issuer_key_generation: issuer.device.key_generation,
            signature: String::new(),
        };
        result.signature = issuer.sign(&result.signing_bytes()?);
        Ok(result)
    }
    pub fn verify_bootstrap(&self, invitation: &Invitation) -> Result<MembershipState> {
        invitation.validate()?;
        if self.state.workspace_id != invitation.workspace_id
            || self.state.revision != invitation.membership_revision
            || self.state.head != invitation.membership_head
            || self.issuer_device_id != invitation.issuer.device_id
            || self.issuer_key_generation != invitation.issuer.key_generation
            || self.state.members.get(&self.issuer_device_id) != Some(&invitation.issuer)
        {
            return Err(Error::Authentication);
        }
        self.verify_signer(&invitation.issuer)?;
        Ok(self.state.clone())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedEnrollment {
    pub version: u8,
    pub invitation_id: String,
    pub candidate_digest: String,
    pub membership: SignedMembership,
    pub signature: String,
}
impl SignedEnrollment {
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        if self.version != VERSION {
            return Err(Error::Unsupported);
        }
        identifier(&self.invitation_id)?;
        frame(
            "zerus-e2ee-enrollment-approval-v1",
            &[
                self.invitation_id.as_bytes(),
                &decode_fixed::<32>(&self.candidate_digest)?,
                &self.membership.statement.canonical_bytes()?,
                &decode_fixed::<64>(&self.membership.signature)?,
            ],
        )
    }
    pub fn sign(
        candidate: &EnrollmentCandidate,
        membership: SignedMembership,
        issuer: &Identity,
    ) -> Result<Self> {
        let mut result = Self {
            version: VERSION,
            invitation_id: candidate.invitation_id.clone(),
            candidate_digest: candidate.digest()?,
            membership,
            signature: String::new(),
        };
        result.signature = issuer.sign(&result.signing_bytes()?);
        result.verify(candidate, issuer.public())?;
        Ok(result)
    }
    /// Public signature/candidate binding only. The approving endpoint must
    /// separately verify the one-time HMAC and the preceding membership head.
    pub fn verify(
        &self,
        candidate: &EnrollmentCandidate,
        active_issuer: &DevicePublic,
    ) -> Result<()> {
        candidate.verify_signature()?;
        if self.invitation_id != candidate.invitation_id
            || self.candidate_digest != candidate.digest()?
            || self.membership.statement.workspace_id != candidate.workspace_id
            || self.membership.statement.action != MembershipAction::Add
            || self.membership.statement.device.as_ref() != Some(&candidate.device)
        {
            return Err(Error::Authentication);
        }
        self.membership.verify_signature(active_issuer)?;
        verify(
            &active_issuer.signing_public_key,
            &self.signing_bytes()?,
            &self.signature,
        )
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentCandidate {
    pub version: u8,
    pub workspace_id: String,
    pub invitation_id: String,
    pub purpose: Role,
    pub device: DevicePublic,
    pub proof: String,
    pub signature: String,
}
impl EnrollmentCandidate {
    pub fn proof_bytes(&self) -> Result<Vec<u8>> {
        if self.version != VERSION {
            return Err(Error::Unsupported);
        }
        identifier(&self.workspace_id)?;
        identifier(&self.invitation_id)?;
        if self.purpose != self.device.role {
            return Err(Error::Invalid);
        }
        frame(
            "zerus-e2ee-enrollment-proof-v1",
            &[
                self.workspace_id.as_bytes(),
                self.invitation_id.as_bytes(),
                self.purpose.name().as_bytes(),
                &self.device.canonical_bytes()?,
            ],
        )
    }
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        frame(
            "zerus-e2ee-enrollment-candidate-v1",
            &[&self.proof_bytes()?, &decode_fixed::<32>(&self.proof)?],
        )
    }
    pub fn verify_signature(&self) -> Result<()> {
        verify(
            &self.device.signing_public_key,
            &self.signing_bytes()?,
            &self.signature,
        )
    }
    pub fn digest(&self) -> Result<String> {
        Ok(encode(&Sha256::digest(frame(
            "zerus-e2ee-candidate-digest-v1",
            &[
                &self.signing_bytes()?,
                &decode_fixed::<64>(&self.signature)?,
            ],
        )?)))
    }
    pub fn create(invitation: &Invitation, identity: &Identity, now: u64) -> Result<Self> {
        invitation.validate()?;
        if now >= invitation.expires_at || invitation.purpose != identity.device.role {
            return Err(Error::Authentication);
        }
        let mut result = Self {
            version: VERSION,
            workspace_id: invitation.workspace_id.clone(),
            invitation_id: invitation.invitation_id.clone(),
            purpose: invitation.purpose,
            device: identity.device.clone(),
            proof: String::new(),
            signature: String::new(),
        };
        let secret = Zeroizing::new(decode_fixed::<32>(&invitation.secret)?);
        let mut mac = HmacSha256::new_from_slice(&*secret).map_err(|_| Error::Invalid)?;
        mac.update(&result.proof_bytes()?);
        result.proof = encode(&mac.finalize().into_bytes());
        result.signature = identity.sign(&result.signing_bytes()?);
        Ok(result)
    }
    /// Online inviter verifies its locally retained one-time invitation. The
    /// invitation consumption and accepted candidate must commit atomically.
    pub fn verify_invitation(&self, invitation: &Invitation, now: u64) -> Result<()> {
        invitation.validate()?;
        if self.workspace_id != invitation.workspace_id
            || self.invitation_id != invitation.invitation_id
            || self.purpose != invitation.purpose
            || now >= invitation.expires_at
        {
            return Err(Error::Authentication);
        }
        self.verify_signature()?;
        let secret = Zeroizing::new(decode_fixed::<32>(&invitation.secret)?);
        let mut mac = HmacSha256::new_from_slice(&*secret).map_err(|_| Error::Invalid)?;
        mac.update(&self.proof_bytes()?);
        mac.verify_slice(&decode_fixed::<32>(&self.proof)?)
            .map_err(|_| Error::Authentication)
    }
}
