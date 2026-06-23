use anyhow::{Result, Context, anyhow};

mod build_composefs_dump;
mod build_etc;
mod generate_units;

const COMMANDS: &str = "Commands: build-composefs-dump, build-etc, generate-units";

fn main() -> Result<()> {
    let mut args = std::env::args();
    let command = args.nth(1).with_context(|| anyhow!("Missing command argument\n{COMMANDS}"))?;

    match command.as_str() {
        "build-composefs-dump" => build_composefs_dump::run(args),
        "build-etc" => build_etc::run(args),
        "generate-units" => generate_units::run(args),
        _ => Err(anyhow!("unknown command: \"{command}\"\n{COMMANDS}"))
    }
}
