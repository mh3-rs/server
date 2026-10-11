//! OPN (TLS, port 8200): the first server a console meets. It checks the disc (game id, then the NAT negotiation
//! hostname our patch writes), then the token of our Nintendo login (`nas.rs`): issued by it, and its Wii not
//! banned. It shows the terms and the announce, then hands out LMP's address.
//!
//! A refused console gets `NtcLogin` status 1, sends its token anyway, asks `ReqMaintenance` and shows the text it
//! gets back: the game's own refusal path (decision D4), for every reason (`PatInterface.cpp:1250-1275`). The token's
//! reasons come after the first `NtcLogin`: the token is answered with a second one, status 1, which the game takes
//! as it does the first (`recvNtcLogin` has no guard), then asks `ReqMaintenance`. The game shows no text for a
//! refused token answer (the console run of 2026-10-11).

use bytes::Bytes;
use tracing::{info, warn};
use wire::packets::*;
use wire::{Frame, Item, PatData};

use super::{Fault, Handled, allowed, frame, reply, unexpected};
use crate::Shared;
use crate::text::{Language, Terms, TermsKind, Text, terms, text};

/// Where every disc keeps its game id (`RMHP08`, …).
const GAME_ID: (u32, u32) = (0x8000_0000, 6);
/// The NAT negotiation hostnames: two NUL-terminated strings (`natneg1…`, `natneg2…`) in 56 bytes.
const NATNEG_HOSTNAMES_SIZE: u32 = 56;

/// `NtcLogin` statuses on OPN: the game's login state (`patState_8254`). 1 is a refusal that ends the login after
/// its text (`isOpeningMaintenanceTerms`); 2 goes on to the terms after it, which a refused console never gets
/// (found in the console run of 2026-10-11: every refusal sent 2 until then, and none ever showed on a console).
const LOGIN_OK: u8 = 5;
const LOGIN_REFUSED: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Region {
    Pal,
    Us,
}

impl Region {
    fn from_game_id(id: &[u8]) -> Option<Self> {
        match id {
            b"RMHP08" => Some(Region::Pal),
            b"RMHE08" => Some(Region::Us),
            _ => None,
        }
    }

    fn natneg_hostnames(self) -> u32 {
        match self {
            Region::Pal => 0x8063_11a8,
            Region::Us => 0x8063_08e8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    GameId,
    NatnegHostname(Region),
    Connection,
    Refused,
    /// The refusal's text sent: the console leaves (`net.rs`, closing).
    Told,
    LoggedIn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    Maintenance,
    NotConfigured,
    NotReady,
    UnsupportedDisc,
    Unpatched,
    NotAllowed,
    /// A token our Nintendo login didn't issue, or expired.
    LoginElsewhere,
    Banned,
}

pub struct Opn {
    stage: Stage,
    region: Option<Region>,
    /// A disc problem found before `AnsConnection`.
    disc: Option<Refusal>,
    refusal: Option<Refusal>,
    language: Language,
    /// Loaded at the first request, so every chunk comes from the same version.
    terms: Option<Terms>,
    subterms: Option<Terms>,
    /// The console's encryption key, given at `ReqCommonKey` (encryption on): it goes on to its LMP connection.
    pub key: Option<crate::keys::Key>,
    /// Where the console connects from: told to LMP when it passed (`Admission`).
    ip: std::net::IpAddr,
}

impl Opn {
    pub fn start(ip: std::net::IpAddr) -> (Self, Handled) {
        let opn = Opn {
            stage: Stage::GameId,
            region: None,
            disc: None,
            refusal: None,
            language: Language::default(),
            terms: None,
            subterms: None,
            key: None,
            ip,
        };
        (opn, memory_check(GAME_ID.0, GAME_ID.1).map(|f| vec![f]))
    }

    pub async fn handle(&mut self, shared: &Shared, req: &Frame) -> Handled {
        let seq = req.seq;
        match (self.stage, req.id) {
            (Stage::GameId, AnsMemoryCheck::ID) => {
                let ans = self.memory(req, GAME_ID.0)?;
                match Region::from_game_id(&ans.memory) {
                    Some(region) => {
                        self.region = Some(region);
                        self.stage = Stage::NatnegHostname(region);
                        Ok(vec![memory_check(
                            region.natneg_hostnames(),
                            NATNEG_HOSTNAMES_SIZE,
                        )?])
                    }
                    None => {
                        self.disc = Some(Refusal::UnsupportedDisc);
                        self.connection()
                    }
                }
            }
            (Stage::NatnegHostname(region), AnsMemoryCheck::ID) => {
                let ans = self.memory(req, region.natneg_hostnames())?;
                let hostname = ans.memory.split(|&b| b == 0).next().unwrap_or_default();
                // Unset: every console is refused at `AnsConnection` anyway (not configured).
                if let Some(accepted) = &shared.config.natneg_hostname
                    && hostname != accepted.as_bytes()
                {
                    self.disc = Some(Refusal::Unpatched);
                }
                self.connection()
            }
            (Stage::Connection, AnsConnection::ID) => {
                let ans = AnsConnection::decode(&req.payload)?;
                self.refusal = self.refusal(shared, &ans.data).await?;
                let status = match self.refusal {
                    Some(reason) => {
                        warn!(?reason, region = ?self.region, "login refused");
                        self.stage = Stage::Refused;
                        LOGIN_REFUSED
                    }
                    None => {
                        info!(region = ?self.region, "reached OPN");
                        self.stage = Stage::LoggedIn;
                        LOGIN_OK
                    }
                };
                reply(seq, &NtcLogin { status })
            }
            // Sent after a refused `NtcLogin` too: acknowledged, the refusal is decided.
            (Stage::Refused, ReqAuthenticationToken::ID) => {
                ReqAuthenticationToken::decode(&req.payload)?;
                reply(seq, &AnsAuthenticationToken {})
            }
            (Stage::Refused, ReqMaintenance::ID) => {
                ReqMaintenance::decode(&req.payload)?;
                self.stage = Stage::Told;
                let text = match self.refusal {
                    Some(Refusal::Maintenance) => {
                        shared.maintenance().unwrap_or_default().to_owned().into()
                    }
                    Some(reason) => text(reason.text(), self.language),
                    None => unreachable!("refused without a reason"),
                };
                reply(
                    seq,
                    &AnsMaintenance {
                        text: Bytes::copy_from_slice(text.as_bytes()),
                    },
                )
            }
            (Stage::LoggedIn, _) => self.request(shared, req).await,
            (_, id) => Err(unexpected(id)),
        }
    }

    async fn request(&mut self, shared: &Shared, req: &Frame) -> Handled {
        let seq = req.seq;
        let p = &req.payload;
        match req.id {
            ReqAuthenticationToken::ID => {
                // Our Nintendo login's token (decision 13), only looked at: LMP uses it up. Never logged.
                let token = ReqAuthenticationToken::decode(p)?.token;
                let Some(device) = shared.nas.get(String::from_utf8_lossy(&token).as_ref()) else {
                    return self.refuse(seq, Refusal::LoginElsewhere);
                };
                let db = shared
                    .db
                    .pool()
                    .ok_or_else(|| Fault::Database(anyhow::anyhow!("database not opened")))?;
                if crate::access::device_banned(db, &device)
                    .await
                    .map_err(Fault::Database)?
                {
                    return self.refuse(seq, Refusal::Banned);
                }
                reply(seq, &AnsAuthenticationToken {})
            }
            ReqTermsVersion::ID => {
                ReqTermsVersion::decode(p)?;
                let terms = self.terms(shared, TermsKind::Terms).await?;
                reply(
                    seq,
                    &AnsTermsVersion {
                        version: terms.version,
                        size: terms.text.len() as u32,
                    },
                )
            }
            ReqTerms::ID => {
                let r = ReqTerms::decode(p)?;
                let terms = self.terms(shared, TermsKind::Terms).await?;
                reply(
                    seq,
                    &AnsTerms {
                        offset: r.offset,
                        size: r.size,
                        chunk: chunk(&terms.text, r.offset, r.size),
                    },
                )
            }
            ReqSubTermsInfo::ID => {
                let r = ReqSubTermsInfo::decode(p)?;
                let terms = self.terms(shared, TermsKind::SubTerms).await?;
                reply(
                    seq,
                    &AnsSubTermsInfo {
                        version: terms.version,
                        index: r.index,
                        size: terms.text.len() as u32,
                    },
                )
            }
            ReqSubTerms::ID => {
                let r = ReqSubTerms::decode(p)?;
                let terms = self.terms(shared, TermsKind::SubTerms).await?;
                reply(
                    seq,
                    &AnsSubTerms {
                        index: r.index,
                        offset: r.offset,
                        size: r.size,
                        chunk: chunk(&terms.text, r.offset, r.size),
                    },
                )
            }
            ReqAnnounce::ID => {
                ReqAnnounce::decode(p)?;
                reply(
                    seq,
                    &AnsAnnounce {
                        text: text(Text::Announce, self.language).into_owned().into(),
                    },
                )
            }
            ReqNoCharge::ID => {
                ReqNoCharge::decode(p)?;
                reply(
                    seq,
                    &AnsNoCharge {
                        text: text(Text::NoCharge, self.language).into_owned().into(),
                    },
                )
            }
            ReqVulgarityInfoLow::ID => {
                let r = ReqVulgarityInfoLow::decode(p)?;
                // An empty list: the console then never asks for it (`ReqVulgarityLow`).
                reply(
                    seq,
                    &AnsVulgarityInfoLow {
                        unk: 1,
                        info: r.info,
                        size: 0,
                    },
                )
            }
            ReqCommonKey::ID => {
                // A key turns on the game's encryption for every later connection; `AnsAuthenticationToken` skips it
                // (MH3SP's answer).
                ReqCommonKey::decode(p)?;
                if shared.keys.is_none() {
                    return reply(seq, &AnsAuthenticationToken {});
                }
                let key = crate::keys::Keys::draw().map_err(|e| Fault::Server(e.into()))?;
                self.key = Some(key);
                reply(
                    seq,
                    &AnsCommonKey {
                        key: Bytes::copy_from_slice(&key),
                    },
                )
            }
            ReqLmpConnect::ID => {
                ReqLmpConnect::decode(p)?;
                let Some(address) = &shared.config.external_address else {
                    // Refused at `AnsConnection` when unset.
                    return Err(Fault::Server(anyhow::anyhow!("external_address unset")));
                };
                shared.admission.passed(self.ip);
                reply(
                    seq,
                    &AnsLmpConnect {
                        address: Bytes::copy_from_slice(address.as_bytes()),
                        port: shared.lmp_port,
                    },
                )
            }
            id => Err(unexpected(id)),
        }
    }

    fn memory(&self, req: &Frame, address: u32) -> Result<AnsMemoryCheck, Fault> {
        let ans = AnsMemoryCheck::decode(&req.payload)?;
        if ans.address != address {
            return Err(Fault::Client(anyhow::anyhow!(
                "memory check answered for {:#x}, asked {address:#x}",
                ans.address
            )));
        }
        Ok(ans)
    }

    /// Whether the refusal's text was sent.
    pub fn finished(&self) -> bool {
        self.stage == Stage::Told
    }

    /// Refuses a console after its first `NtcLogin`: a second one, status 1, in place of the answer (module doc).
    fn refuse(&mut self, seq: u16, reason: Refusal) -> Handled {
        warn!(?reason, region = ?self.region, "login refused");
        self.refusal = Some(reason);
        self.stage = Stage::Refused;
        reply(
            seq,
            &NtcLogin {
                status: LOGIN_REFUSED,
            },
        )
    }

    /// The disc checks are done: ask for the connection data.
    fn connection(&mut self) -> Handled {
        self.stage = Stage::Connection;
        Ok(vec![super::req_connection()?])
    }

    async fn refusal(&self, shared: &Shared, data: &PatData) -> Result<Option<Refusal>, Fault> {
        if shared.maintenance().is_some() {
            return Ok(Some(Refusal::Maintenance));
        }
        if !shared.configured() {
            return Ok(Some(Refusal::NotConfigured));
        }
        if let Some(disc) = self.disc {
            return Ok(Some(disc));
        }
        if shared.db.not_ready().await.is_some() {
            return Ok(Some(Refusal::NotReady));
        }
        let db = shared.db.pool().expect("ready, so opened");
        let support_code = match data.get(0x01) {
            Some(Item::String(s)) => Some(&s[..]),
            _ => None,
        };
        Ok((!allowed(db, support_code).await?).then_some(Refusal::NotAllowed))
    }

    async fn terms(&mut self, shared: &Shared, kind: TermsKind) -> Result<&Terms, Fault> {
        let slot = match kind {
            TermsKind::Terms => &mut self.terms,
            TermsKind::SubTerms => &mut self.subterms,
        };
        if slot.is_none() {
            let db = shared
                .db
                .pool()
                .ok_or_else(|| Fault::Database(anyhow::anyhow!("database not opened")))?;
            *slot = Some(terms(db, kind, self.language).await?);
        }
        Ok(slot.as_ref().expect("just loaded"))
    }
}

impl Refusal {
    fn text(self) -> Text {
        match self {
            Refusal::Maintenance => unreachable!("maintenance has its own message"),
            Refusal::NotConfigured => Text::NotConfigured,
            Refusal::NotReady => Text::NotReady,
            Refusal::UnsupportedDisc => Text::UnsupportedDisc,
            Refusal::Unpatched => Text::Unpatched,
            Refusal::NotAllowed => Text::NotAllowed,
            Refusal::LoginElsewhere => Text::LoginElsewhere,
            Refusal::Banned => Text::WiiBanned,
        }
    }
}

fn memory_check(address: u32, size: u32) -> Result<Frame, Fault> {
    // MH3SP's memory data: one byte field (what it means is unknown).
    let data = PatData::new().with(0x01, Item::Byte(0));
    frame(
        0,
        &ReqMemoryCheck {
            data,
            address,
            size,
        },
    )
}

/// `size` bytes of `text` from `offset`, as many as there are.
fn chunk(text: &str, offset: u32, size: u32) -> Bytes {
    let bytes = text.as_bytes();
    let start = (offset as usize).min(bytes.len());
    let end = start.saturating_add(size as usize).min(bytes.len());
    Bytes::copy_from_slice(&bytes[start..end])
}
