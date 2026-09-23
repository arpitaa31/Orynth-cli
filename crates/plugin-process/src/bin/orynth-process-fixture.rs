use std::{env, io, process, thread, time::Duration};

use orynth_plugin_api::PluginResourceLimits;
use orynth_plugin_process::wire::{self, WireResponse};

fn main() {
    let arguments: Vec<String> = env::args().collect();
    let mut input = io::stdin().lock();
    let request = match wire::read_request(&mut input, PluginResourceLimits::default()) {
        Ok(request) => request,
        Err(_) => process::exit(2),
    };
    if arguments.iter().any(|argument| argument == "--crash") {
        process::exit(17);
    }
    if arguments.iter().any(|argument| argument == "--hang") {
        thread::sleep(Duration::from_secs(60));
    }
    let mut output = io::stdout().lock();
    if wire::write_response(
        &mut output,
        request.request.request_id,
        &WireResponse::Success(request.request.payload),
        PluginResourceLimits::default(),
    )
    .is_err()
    {
        process::exit(3);
    }
}
