use crate::config::CliConfig;
use crate::output::Output;
use anyhow::Result;

pub async fn handle(args: &super::ConfigArgs, output: &Output) -> Result<()> {
    match &args.command {
        super::ConfigCommands::Show => {
            let config = CliConfig::load()?;
            output.emit_json(&config)
        }
        super::ConfigCommands::Set { key, value } => {
            let mut config = CliConfig::load()?;
            match key {
                super::ConfigKeyArg::Url => config.url = Some(value.clone()),
                super::ConfigKeyArg::Keystore => config.keystore = Some(value.clone()),
                super::ConfigKeyArg::DefaultSpace => config.default_space = Some(value.clone()),
            }
            config.save()?;
            output.emit_json(&config)
        }
        super::ConfigCommands::Reset => {
            CliConfig::reset()?;
            output.emit_text("Config reset")
        }
    }
}
