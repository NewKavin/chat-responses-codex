use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use std::io;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct ScramSha256Client {
    password: String,
    client_nonce: String,
    client_first_message_bare: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScramSha256Exchange {
    pub client_final_message: String,
    pub expected_server_signature: String,
}

impl ScramSha256Client {
    pub(crate) fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::with_nonce(username, password, generate_nonce())
    }

    pub(crate) fn with_nonce(
        username: impl Into<String>,
        password: impl Into<String>,
        nonce: impl Into<String>,
    ) -> Self {
        let username = username.into();
        let client_nonce = nonce.into();
        let client_first_message_bare = format!(
            "n={},r={}",
            escape_username(&username),
            client_nonce.as_str()
        );

        Self {
            password: password.into(),
            client_nonce,
            client_first_message_bare,
        }
    }

    pub(crate) fn client_first_message(&self) -> String {
        format!("n,,{}", self.client_first_message_bare)
    }

    pub(crate) fn process_server_first_message(
        &self,
        server_first_message: &str,
    ) -> io::Result<ScramSha256Exchange> {
        let server_first = ServerFirstMessage::parse(server_first_message)?;
        if !server_first.nonce.starts_with(&self.client_nonce) {
            return Err(invalid_scram(
                "postgres scram server nonce does not extend the client nonce",
            ));
        }

        let client_final_without_proof = format!("c=biws,r={}", server_first.nonce);
        let auth_message = format!(
            "{},{},{}",
            self.client_first_message_bare, server_first_message, client_final_without_proof
        );
        let salted_password = hi(
            self.password.as_bytes(),
            &server_first.salt,
            server_first.iterations,
        )?;
        let client_key = hmac_sha256(&salted_password, b"Client Key");
        let stored_key = sha256(&client_key);
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let client_proof = xor_bytes(&client_key, &client_signature);
        let server_key = hmac_sha256(&salted_password, b"Server Key");
        let server_signature = hmac_sha256(&server_key, auth_message.as_bytes());

        Ok(ScramSha256Exchange {
            client_final_message: format!(
                "{},p={}",
                client_final_without_proof,
                STANDARD.encode(client_proof)
            ),
            expected_server_signature: STANDARD.encode(server_signature),
        })
    }
}

pub(crate) fn verify_server_final_message(
    server_final_message: &str,
    expected_server_signature: &str,
) -> io::Result<()> {
    let parsed = ServerFinalMessage::parse(server_final_message)?;
    let signature = parsed.signature.ok_or_else(|| {
        invalid_scram("postgres scram server-final message did not include a signature")
    })?;
    if !constant_time_eq(signature.as_bytes(), expected_server_signature.as_bytes()) {
        return Err(invalid_scram(
            "postgres scram server signature did not match the expected value",
        ));
    }
    Ok(())
}

struct ServerFirstMessage {
    nonce: String,
    salt: Vec<u8>,
    iterations: u32,
}

impl ServerFirstMessage {
    fn parse(input: &str) -> io::Result<Self> {
        let mut nonce = None;
        let mut salt = None;
        let mut iterations = None;

        for (key, value) in parse_scram_attributes(input)? {
            match key.as_str() {
                "r" => nonce = Some(value),
                "s" => {
                    salt = Some(
                        STANDARD
                            .decode(value.as_bytes())
                            .map_err(|error| invalid_scram(&error.to_string()))?,
                    )
                }
                "i" => {
                    let parsed = value
                        .parse::<u32>()
                        .map_err(|error| invalid_scram(&error.to_string()))?;
                    if parsed == 0 {
                        return Err(invalid_scram(
                            "postgres scram iteration count must be greater than zero",
                        ));
                    }
                    iterations = Some(parsed);
                }
                "m" => {
                    return Err(invalid_scram(
                        "postgres scram server sent an unsupported attribute",
                    ));
                }
                "e" => {
                    return Err(invalid_scram(&format!(
                        "postgres scram server returned an error: {value}"
                    )));
                }
                other => {
                    return Err(invalid_scram(&format!(
                        "postgres scram server sent an unexpected attribute: {other}"
                    )));
                }
            }
        }

        Ok(Self {
            nonce: nonce
                .ok_or_else(|| invalid_scram("postgres scram server message missing nonce"))?,
            salt: salt
                .ok_or_else(|| invalid_scram("postgres scram server message missing salt"))?,
            iterations: iterations.ok_or_else(|| {
                invalid_scram("postgres scram server message missing iteration count")
            })?,
        })
    }
}

struct ServerFinalMessage {
    signature: Option<String>,
}

impl ServerFinalMessage {
    fn parse(input: &str) -> io::Result<Self> {
        let mut signature = None;

        for (key, value) in parse_scram_attributes(input)? {
            match key.as_str() {
                "v" => signature = Some(value),
                "e" => {
                    return Err(invalid_scram(&format!(
                        "postgres scram server returned an error: {value}"
                    )));
                }
                "m" => {
                    return Err(invalid_scram(
                        "postgres scram server sent an unsupported attribute",
                    ));
                }
                other => {
                    return Err(invalid_scram(&format!(
                        "postgres scram server sent an unexpected attribute: {other}"
                    )));
                }
            }
        }

        Ok(Self { signature })
    }
}

fn parse_scram_attributes(input: &str) -> io::Result<Vec<(String, String)>> {
    if input.is_empty() {
        return Err(invalid_scram("postgres scram message was empty"));
    }

    let mut attributes = Vec::new();
    for item in input.split(',') {
        let (key, value) = item
            .split_once('=')
            .ok_or_else(|| invalid_scram("postgres scram attribute missing equals sign"))?;
        if key.is_empty() {
            return Err(invalid_scram("postgres scram attribute key was empty"));
        }
        attributes.push((key.to_string(), unescape_scram_value(value)?));
    }
    Ok(attributes)
}

fn unescape_scram_value(input: &str) -> io::Result<String> {
    let mut output = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte != b'=' {
            output.push(byte as char);
            index += 1;
            continue;
        }

        if index + 2 < bytes.len() {
            match (bytes[index + 1], bytes[index + 2]) {
                (b'2', b'c') | (b'2', b'C') => {
                    output.push(',');
                    index += 3;
                    continue;
                }
                (b'3', b'd') | (b'3', b'D') => {
                    output.push('=');
                    index += 3;
                    continue;
                }
                _ => {}
            }
        }

        if index + 2 >= bytes.len() {
            output.push('=');
            index += 1;
            continue;
        }

        match (bytes[index + 1], bytes[index + 2]) {
            (b'2', b'c') | (b'2', b'C') => output.push(','),
            (b'3', b'd') | (b'3', b'D') => output.push('='),
            _ => {
                output.push('=');
                output.push(bytes[index + 1] as char);
                output.push(bytes[index + 2] as char);
            }
        }
        index += 3;
    }

    Ok(output)
}

fn escape_username(username: &str) -> String {
    let mut escaped = String::with_capacity(username.len());
    for ch in username.chars() {
        match ch {
            ',' => escaped.push_str("=2C"),
            '=' => escaped.push_str("=3D"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn generate_nonce() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn hi(password: &[u8], salt: &[u8], iterations: u32) -> io::Result<[u8; 32]> {
    if iterations == 0 {
        return Err(invalid_scram(
            "postgres scram iteration count must be positive",
        ));
    }

    let mut salted = Vec::with_capacity(salt.len() + 4);
    salted.extend_from_slice(salt);
    salted.extend_from_slice(&1u32.to_be_bytes());

    let mut u = hmac_sha256(password, &salted);
    let mut output = u;

    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        xor_into(&mut output, &u);
    }

    Ok(output)
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut normalized_key = [0u8; BLOCK_SIZE];

    if key.len() > BLOCK_SIZE {
        normalized_key.copy_from_slice(&sha256(key));
    } else {
        normalized_key[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36u8; BLOCK_SIZE];
    let mut outer_pad = [0x5cu8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_pad[index] ^= normalized_key[index];
        outer_pad[index] ^= normalized_key[index];
    }

    let mut inner = Vec::with_capacity(BLOCK_SIZE + data.len());
    inner.extend_from_slice(&inner_pad);
    inner.extend_from_slice(data);
    let inner_hash = sha256(&inner);

    let mut outer = Vec::with_capacity(BLOCK_SIZE + inner_hash.len());
    outer.extend_from_slice(&outer_pad);
    outer.extend_from_slice(&inner_hash);
    sha256(&outer)
}

fn xor_into(left: &mut [u8], right: &[u8]) {
    for (left, right) in left.iter_mut().zip(right.iter().copied()) {
        *left ^= right;
    }
}

fn xor_bytes(left: &[u8], right: &[u8]) -> [u8; 32] {
    let mut output = [0u8; 32];
    for index in 0..output.len() {
        output[index] = left[index] ^ right[index];
    }
    output
}

fn sha256(data: &[u8]) -> [u8; 32] {
    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (data.len() as u64) * 8;
    let mut message = Vec::with_capacity(data.len() + 1 + 8 + 64);
    message.extend_from_slice(data);
    message.push(0x80);
    while (message.len() % 64) != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = H0;
    let mut schedule = [0u32; 64];

    for chunk in message.chunks_exact(64) {
        for (index, word) in schedule.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }

        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];

        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut output = [0u8; 32];
    for (index, value) in state.iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    output
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut diff = 0u8;
    for (left, right) in left.iter().zip(right.iter()) {
        diff |= *left ^ *right;
    }
    diff == 0
}

fn invalid_scram(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
