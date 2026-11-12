//! CLI options and local environment configuration.
use anyhow::{Result, anyhow, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum, parser::ValueSource};
use mimicmail::pipeline;
use std::{collections::HashMap, io::ErrorKind, path::PathBuf};

#[derive(Parser)]
#[command(version, about = "과거 발신 메일의 문체를 참고하는 이메일 초안 생성기")]
pub struct Args {
    #[arg(long, default_value = "data/mimicmail.sqlite3")]
    pub db: PathBuf,
    #[arg(long)]
    pub user: Option<String>,
    #[arg(long, value_enum, default_value = "codex")]
    pub backend: Backend,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(skip)]
    pub api_key: String,
    #[command(subcommand)]
    pub command: Commands,
}
#[derive(Clone, ValueEnum)]
pub enum Backend {
    Codex,
    Openai,
}
#[derive(Subcommand)]
pub enum Commands {
    #[command(about = "본인 발신 .eml을 분류하고 저장")]
    Ingest {
        #[arg(required=true,num_args=1..)]
        paths: Vec<PathBuf>,
        #[arg(long)]
        alias: Vec<String>,
        #[arg(long)]
        since: Option<chrono::NaiveDate>,
    },
    #[command(about = "현재 대화를 바탕으로 초안 생성")]
    Draft {
        eml: PathBuf,
        #[arg(long)]
        to: String,
        #[arg(long)]
        request: String,
        #[arg(long)]
        role: Option<String>,
        #[arg(long,value_parser=pipeline::RELATIONSHIPS)]
        relationship: Option<String>,
        #[arg(long,default_value="5",value_parser=clap::value_parser!(u32).range(1..))]
        limit: u32,
    },
    #[command(about = "DB에서 대화·직급·문체 예시를 읽기 전용으로 조회")]
    Context {
        #[arg(long)]
        to: String,
        #[arg(long,default_value="other",value_parser=pipeline::PURPOSES)]
        purpose: String,
        #[arg(long)]
        history_key: Vec<String>,
        #[arg(long)]
        style_id: Vec<String>,
    },
    #[command(about = "로컬 메일 보관함·편집기 열기")]
    Gui {
        #[arg(long, default_value = "8765")]
        port: u16,
        #[arg(long)]
        no_open: bool,
    },
}

impl Args {
    pub fn load() -> Result<Self> {
        // Parse first so --help and --version never need configuration or credentials.
        let matches = Self::command().get_matches();
        let mut args = Self::from_arg_matches(&matches)?;
        let mut file = HashMap::new();
        // Read only the working directory; never import a parent project's secrets.
        // Iteration avoids mutating the process environment or exporting keys to children.
        match dotenvy::from_path_iter(".env") {
            Ok(entries) => {
                for entry in entries {
                    let (key, value) =
                        entry.map_err(|_| anyhow!(".env 파일의 형식과 읽기 권한을 확인하세요."))?;
                    file.entry(key).or_insert(value);
                }
            }
            Err(dotenvy::Error::Io(error)) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => bail!(".env 파일의 형식과 읽기 권한을 확인하세요."),
        }
        let value = |key| std::env::var(key).ok().or_else(|| file.get(key).cloned());
        if matches.value_source("db") != Some(ValueSource::CommandLine)
            && let Some(path) = value("MIMICMAIL_DB")
        {
            args.db = PathBuf::from(path);
        }
        if args.user.is_none() {
            args.user = value("MIMICMAIL_USER");
        }
        if matches.value_source("backend") != Some(ValueSource::CommandLine)
            && let Some(backend) = value("MIMICMAIL_BACKEND")
        {
            args.backend = Backend::from_str(&backend, false)
                .map_err(|_| anyhow!("MIMICMAIL_BACKEND를 codex 또는 openai로 설정하세요."))?;
        }
        if args.model.is_none() {
            args.model = value(match args.backend {
                Backend::Codex => "CODEX_MODEL",
                Backend::Openai => "OPENAI_MODEL",
            });
        }
        args.api_key = value("OPENAI_API_KEY").unwrap_or_default();
        if let Commands::Gui { port, .. } = &mut args.command
            && matches
                .subcommand_matches("gui")
                .unwrap()
                .value_source("port")
                != Some(ValueSource::CommandLine)
            && let Some(value) = value("MIMICMAIL_PORT")
        {
            *port = value
                .parse()
                .map_err(|_| anyhow!("MIMICMAIL_PORT를 0~65535 사이의 정수로 설정하세요."))?;
        }
        Ok(args)
    }
}
