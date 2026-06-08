//! CLI subcommands: `genkey`, `gen-service-secret` (no-args invocations of the binary).

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::SigningKey;
use rand::RngCore;
use std::process::ExitCode;

pub fn run(arg: &str) -> ExitCode {
    match arg {
        "genkey" => {
            genkey();
            ExitCode::SUCCESS
        }
        "gen-service-secret" => {
            gen_service_secret();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            eprintln!("usage: provider-stack [genkey|gen-service-secret]");
            ExitCode::from(2)
        }
    }
}

fn genkey() {
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    let signing = SigningKey::from_bytes(&seed);
    let verifying = signing.verifying_key();
    let secret = stellar_strkey::ed25519::PrivateKey(signing.to_bytes()).to_string();
    let public = stellar_strkey::ed25519::PublicKey(verifying.to_bytes()).to_string();
    println!("PP_SECRET_KEY={secret}");
    println!("PP_PUBLIC_KEY={public}");
}

fn gen_service_secret() {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    println!("SERVICE_AUTH_SECRET={}", B64.encode(buf));
}
