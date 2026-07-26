use std::fmt;

use anyhow::{bail, Context, Result};
use reqwest::Url;
use taiko_multiplayer_protocol::{InvitationToken, RoomCode};

const INVITE_SCHEME: &str = "taiko";
const INVITE_HOST: &str = "join";

#[derive(Clone, PartialEq, Eq)]
pub struct MultiplayerInvite {
    server: Url,
    room_code: RoomCode,
    invitation_token: InvitationToken,
}

impl MultiplayerInvite {
    pub fn normalize_server(raw: &str) -> Result<Url> {
        normalize_server_url(Url::parse(raw).context("invalid multiplayer server URL")?)
    }

    pub fn new(
        server: Url,
        room_code: RoomCode,
        invitation_token: InvitationToken,
    ) -> Result<Self> {
        let server = normalize_server_url(server)?;
        Ok(Self {
            server,
            room_code,
            invitation_token,
        })
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let invite = Url::parse(raw).context("invalid multiplayer invite URL")?;
        if invite.scheme() != INVITE_SCHEME
            || invite.host_str() != Some(INVITE_HOST)
            || invite.path() != ""
            || invite.fragment().is_some()
        {
            bail!("invite must use the form taiko://join?server=...&room=...&token=...");
        }

        let mut server = None;
        let mut room = None;
        let mut token = None;
        for (key, value) in invite.query_pairs() {
            let target = match key.as_ref() {
                "server" => &mut server,
                "room" => &mut room,
                "token" => &mut token,
                unknown => bail!("unknown invite parameter `{unknown}`"),
            };
            if target.replace(value.into_owned()).is_some() {
                bail!("duplicate invite parameter `{key}`");
            }
        }

        let server = Url::parse(
            server
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("invite is missing `server`"))?,
        )
        .context("invite contains an invalid server URL")?;
        let room_code =
            RoomCode::parse(room.ok_or_else(|| anyhow::anyhow!("invite is missing `room`"))?)
                .context("invite contains an invalid room code")?;
        let invitation_token = InvitationToken::parse(
            token.ok_or_else(|| anyhow::anyhow!("invite is missing `token`"))?,
        )
        .context("invite contains an invalid invitation token")?;

        Self::new(server, room_code, invitation_token)
    }

    pub fn server(&self) -> &Url {
        &self.server
    }

    pub fn room_code(&self) -> &RoomCode {
        &self.room_code
    }

    pub fn invitation_token(&self) -> &InvitationToken {
        &self.invitation_token
    }

    pub fn to_url(&self) -> Url {
        let mut invite = Url::parse("taiko://join").expect("static invite base URL is valid");
        invite
            .query_pairs_mut()
            .append_pair("server", self.server.as_str())
            .append_pair("room", self.room_code.as_str())
            .append_pair("token", self.invitation_token.expose());
        invite
    }
}

impl fmt::Display for MultiplayerInvite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_url().fmt(f)
    }
}

impl fmt::Debug for MultiplayerInvite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MultiplayerInvite")
            .field("server", &self.server)
            .field("room_code", &self.room_code)
            .field("invitation_token", &"REDACTED")
            .finish()
    }
}

fn normalize_server_url(mut server: Url) -> Result<Url> {
    if !matches!(server.scheme(), "http" | "https") {
        bail!("multiplayer server URL must use http or https");
    }
    if server.host_str().is_none() {
        bail!("multiplayer server URL must include a host");
    }
    if !server.username().is_empty() || server.password().is_some() {
        bail!("multiplayer server URL cannot contain credentials");
    }
    if server.query().is_some() || server.fragment().is_some() {
        bail!("multiplayer server URL cannot contain a query or fragment");
    }

    let mut path = server.path().trim_end_matches('/').to_owned();
    path.push('/');
    server.set_path(&path);
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> InvitationToken {
        InvitationToken::parse("a".repeat(64)).expect("fixture token")
    }

    #[test]
    fn invite_round_trip_is_canonical() {
        let invite = MultiplayerInvite::new(
            Url::parse("https://example.com/taiko").expect("server"),
            RoomCode::parse("J7K9").expect("room"),
            token(),
        )
        .expect("invite");

        let encoded = invite.to_string();
        let decoded = MultiplayerInvite::parse(&encoded).expect("decode");
        assert_eq!(decoded, invite);
        assert_eq!(decoded.server().as_str(), "https://example.com/taiko/");
        assert!(!format!("{decoded:?}").contains(token().expose()));
    }

    #[test]
    fn invite_rejects_missing_duplicate_unknown_and_bad_server_fields() {
        let secret = "a".repeat(64);
        let cases = vec![
            "taiko://join?server=https%3A%2F%2Fexample.com&room=J7K9".to_owned(),
            format!(
                "taiko://join?server=https%3A%2F%2Fexample.com&room=J7K9&room=ABCD&token={secret}"
            ),
            format!(
                "taiko://join?server=https%3A%2F%2Fexample.com&room=J7K9&token={secret}&legacy=1"
            ),
            format!("taiko://join?server=file%3A%2F%2F%2Ftmp&room=J7K9&token={secret}"),
            format!(
                "taiko://join?server=https%3A%2F%2Fuser%40example.com&room=J7K9&token={secret}"
            ),
        ];
        for raw in cases {
            assert!(
                MultiplayerInvite::parse(&raw).is_err(),
                "unexpectedly accepted {raw}"
            );
        }
    }

    #[test]
    fn invite_rejects_invalid_room_or_token() {
        let bad_room =
            "taiko://join?server=https%3A%2F%2Fexample.com&room=A0CD&token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let bad_token = "taiko://join?server=https%3A%2F%2Fexample.com&room=J7K9&token=short";
        assert!(MultiplayerInvite::parse(bad_room).is_err());
        assert!(MultiplayerInvite::parse(bad_token).is_err());
    }
}
