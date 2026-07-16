use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub struct ThinkingSignatureInput<'a> {
    pub thinking: &'a str,
    pub model: &'a str,
    pub upstream_id: &'a str,
    pub protocol: &'a str,
    pub profile_fingerprint: &'a str,
    pub call_ids: &'a [&'a str],
}

fn route_binding(secret: &[u8], upstream_id: &str, protocol: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(b"chat2responses/claude-thinking-route/v1\0");
    update_len_prefixed(&mut mac, upstream_id.as_bytes());
    update_len_prefixed(&mut mac, protocol.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn update_len_prefixed(mac: &mut Hmac<Sha256>, value: &[u8]) {
    mac.update(&(value.len() as u64).to_be_bytes());
    mac.update(value);
}

fn thinking_mac(secret: &[u8], input: &ThinkingSignatureInput<'_>) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(b"chat2responses/claude-thinking/v1\0");
    for value in [
        input.thinking,
        input.model,
        input.upstream_id,
        input.protocol,
        input.profile_fingerprint,
    ] {
        update_len_prefixed(&mut mac, value.as_bytes());
    }
    mac.update(&(input.call_ids.len() as u64).to_be_bytes());
    for call_id in input.call_ids {
        update_len_prefixed(&mut mac, call_id.as_bytes());
    }
    mac.finalize().into_bytes().to_vec()
}

pub fn sign_thinking(secret: &[u8], input: &ThinkingSignatureInput<'_>) -> String {
    format!(
        "gw1.{}.{}",
        URL_SAFE_NO_PAD.encode(route_binding(secret, input.upstream_id, input.protocol)),
        URL_SAFE_NO_PAD.encode(thinking_mac(secret, input))
    )
}

pub fn verify_thinking(secret: &[u8], input: &ThinkingSignatureInput<'_>, signature: &str) -> bool {
    let Some(encoded) = signature.strip_prefix("gw1.") else {
        return false;
    };
    let parts = encoded.split('.').collect::<Vec<_>>();
    let encoded_mac = match parts.as_slice() {
        [legacy_mac] => *legacy_mac,
        [encoded_route, encoded_mac] => {
            let Ok(signed_route) = URL_SAFE_NO_PAD.decode(encoded_route) else {
                return false;
            };
            let expected_route = route_binding(secret, input.upstream_id, input.protocol);
            if !bool::from(signed_route.as_slice().ct_eq(expected_route.as_slice())) {
                return false;
            }
            *encoded_mac
        }
        _ => return false,
    };
    let Ok(expected) = URL_SAFE_NO_PAD.decode(encoded_mac) else {
        return false;
    };
    let generated = thinking_mac(secret, input);
    expected.as_slice().ct_eq(generated.as_slice()).into()
}
