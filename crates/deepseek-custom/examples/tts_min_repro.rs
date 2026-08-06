//! Throwaway minimal repro: call kokoro_en::KokoroTts directly, no cpal, no
//! worker thread, no TtsHandle. Used to isolate whether the access
//! violation / "GetElementType is not implemented" error seen in tts_smoke
//! lives in the kokoro-en crate or in our code, and if so which call
//! sequence triggers it.
//!
//! Usage:
//!   cargo run --example tts_min_repro -- <model_path> <voices_path>
//!
//! Runs a fixed bisection sequence: several same-speed calls back to back,
//! then a speed change, then more calls, printing after each so the exact
//! call that fails is visible in the log rather than just "some later one".

use std::env;

use kokoro_en::{KokoroTts, Voice};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let mut args = env::args().skip(1);
    let model_path = args.next().expect("model_path arg");
    let voices_path = args.next().expect("voices_path arg");

    println!("loading model: {model_path}");
    let tts = KokoroTts::new(&model_path, &voices_path).await?;
    println!("model loaded, running bisection sequence...");

    // Steps 1-3: repeat the exact same call (default speed 1.0) several
    // times in a row. If this alone fails, repetition itself is the
    // trigger and set_speed is not involved.
    for i in 1..=3 {
        run_step(&tts, &format!("same-speed call #{i}"), "af_heart", 1.0).await?;
    }

    // Step 4: change speed. If this call itself fails, the speed value
    // passed into this call is the trigger.
    run_step(&tts, "speed-change call (1.8)", "af_heart", 1.8).await?;

    // Step 5: a normal-speed call made right after a speed change. If step
    // 4 succeeds but this one fails, the trigger is "a call following a
    // different-speed call", not the speed value itself.
    run_step(&tts, "post-speed-change call (1.0)", "af_heart", 1.0).await?;

    // Step 6: one more speed change followed immediately by another speed
    // change, no same-speed call in between.
    run_step(&tts, "second speed-change call (1.5)", "af_heart", 1.5).await?;
    run_step(&tts, "third speed-change call (0.8)", "af_heart", 0.8).await?;

    println!("done, no crash across all {} steps", 7);

    Ok(())
}

/// Run one synth call, label it in the log, and report sample count and
/// timing so a human can see exactly which step failed.
async fn run_step(
    tts: &KokoroTts,
    label: &str,
    voice_id: &str,
    speed: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("-- {label} --");
    let voice = Voice::new(voice_id).with_speed(speed);
    let (audio, took) = tts
        .synth(
            "Hello. This is a short line to test text to speech playback.",
            voice,
        )
        .await?;
    println!(
        "  ok: synth took {took:?}, produced {} samples",
        audio.len()
    );
    Ok(())
}
