//! A virtual Livid Block for testing MIDI without hardware (macOS/Linux).
//!
//! Creates a virtual MIDI source and destination named "Virtual Block" (or the
//! first argument). Its name contains "block", so a running 4sd auto-connects
//! to it like the real device -- which also exercises hotplug detection.
//!
//! - Prints every message 4sd sends to it (LED updates) as hex.
//! - Reads commands from stdin and sends them to 4sd:
//!     pad ROW COL      press + release the pad (0-based, default note map)
//!     knob INDEX VALUE CC for knob INDEX (0-based), VALUE 0-127
//!     raw HEX..        arbitrary bytes, e.g. `raw 90 00 7f`
//!
//! Run: cargo run -p fours-daemon --example virtual_block

use midir::os::unix::{VirtualInput, VirtualOutput};
use midir::{MidiInput, MidiOutput};
use std::io::{BufRead, Write};

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "Virtual Block".into());
    let mut out = MidiOutput::new("4S virtual block")
        .unwrap()
        .create_virtual(&name)
        .expect("create virtual source");
    let _in = MidiInput::new("4S virtual block")
        .unwrap()
        .create_virtual(
            &name,
            |_t, msg, _| {
                let hex: Vec<String> = msg.iter().map(|b| format!("{b:02X}")).collect();
                println!("recv {}", hex.join(" "));
                let _ = std::io::stdout().flush();
            },
            (),
        )
        .expect("create virtual destination");
    println!("ready: {name}");
    let _ = std::io::stdout().flush();

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let parts: Vec<&str> = line.split_whitespace().collect();
        let msgs: Vec<Vec<u8>> = match parts.as_slice() {
            ["pad", r, c] => {
                // The default map numbers pads down each column.
                let note = c.parse::<u8>().unwrap_or(0) * 8 + r.parse::<u8>().unwrap_or(0);
                vec![vec![0x90, note, 127], vec![0x80, note, 0]]
            }
            ["knob", i, v] => vec![vec![0xB0, 1 + i.parse::<u8>().unwrap_or(0), v.parse().unwrap_or(0)]],
            ["raw", bytes @ ..] => vec![bytes.iter().filter_map(|b| u8::from_str_radix(b, 16).ok()).collect()],
            [] => continue,
            _ => {
                eprintln!("commands: pad ROW COL | knob INDEX VALUE | raw HEX..");
                continue;
            }
        };
        for m in msgs {
            out.send(&m).expect("send");
        }
        println!("sent");
        let _ = std::io::stdout().flush();
    }
}
