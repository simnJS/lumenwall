//! Drives a running Lumenwall daemon over IPC (manual tests).
//!
//! ```text
//! cargo run -p lumenwall --example lumenctl -- state
//! cargo run -p lumenwall --example lumenctl -- apply /path/to/wallpaper [OUTPUT]
//! cargo run -p lumenwall --example lumenctl -- props OUTPUT | set OUTPUT key=value [--persist]
//! cargo run -p lumenwall --example lumenctl -- pause | resume | mute | unmute | volume 0.3
//! cargo run -p lumenwall --example lumenctl -- settings | fps 20 | stats [SECONDS] | outputs | shutdown
//! ```

use std::time::Duration;

use lumenwall_ipc::{
    Client, ClientEvent, Endpoint, Event, PropertyMap, PropertyValue, Scaling, Topic,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or_default();
    let client = Client::connect_with_events(&Endpoint::for_current_user(), "lumenctl", |e| {
        if let ClientEvent::Event(Event::Stats(s)) = e {
            let gpu_ms = s
                .gpu_percent
                .filter(|_| s.fps > 0.0)
                .map(|g| g * 10.0 / s.fps);
            println!(
                "stats: {:.1} fps, CPU {:.2} ms/frame, GPU {}, process CPU {:.2} %, RSS {:.1} MiB, VRAM {}",
                s.fps,
                s.frame_time_ms,
                gpu_ms.map_or("n/a".into(), |g| format!("{g:.2} ms/frame")),
                s.cpu_percent,
                s.rss_bytes as f64 / 1048576.0,
                s.vram_bytes
                    .map_or("n/a".into(), |v| format!("{:.1} MiB", v as f64 / 1048576.0)),
            );
        }
    })?;
    match arg(0) {
        "state" => println!("{:#?}", client.state()?),
        "outputs" => println!("{:#?}", client.outputs()?),
        "settings" => println!("{:#?}", client.settings()?),
        "apply" => {
            let output = args.get(2).map(String::as_str);
            client.apply(output, arg(1), Scaling::Fill)?;
            println!("{:#?}", client.state()?);
        }
        "clear" => client.clear(args.get(1).map(String::as_str))?,
        "props" => println!("{:#?}", client.properties(arg(1))?),
        "set" => {
            let (key, val) = arg(2).split_once('=').ok_or("key=value")?;
            let value = match val {
                "true" => PropertyValue::Bool(true),
                "false" => PropertyValue::Bool(false),
                v => v
                    .parse::<f64>()
                    .map_or(PropertyValue::Text(v.to_owned()), PropertyValue::Number),
            };
            let values: PropertyMap = [(key.to_owned(), value)].into();
            client.set_properties(arg(1), values, arg(3) == "--persist")?;
        }
        "reset" => client.reset_properties(arg(1))?,
        "pause" => client.set_paused(true)?,
        "resume" => client.set_paused(false)?,
        "mute" => client.set_muted(true)?,
        "unmute" => client.set_muted(false)?,
        "volume" => client.set_volume(arg(1).parse()?)?,
        "fps" => {
            let mut s = client.settings()?;
            s.fps_limit = arg(1).parse()?;
            client.set_settings(s)?;
        }
        "stats" => {
            let secs: u64 = arg(1).parse().unwrap_or(5);
            client.subscribe(&[Topic::Stats])?;
            std::thread::sleep(Duration::from_secs(secs));
            client.unsubscribe(&[Topic::Stats])?;
        }
        "stat" => println!("{:#?}", client.stats()?),
        "shutdown" => client.shutdown()?,
        other => return Err(format!("unknown command {other:?}").into()),
    }
    Ok(())
}
