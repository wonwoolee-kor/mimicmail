use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use mimicmail::{
    db, gui,
    llm::{Codex, LanguageModel, OpenAI},
    mail, pipeline, server,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "과거 발신 메일의 문체를 참고하는 이메일 초안 생성기")]
struct Args {
    #[arg(long, default_value = "data/mimicmail.sqlite3")]
    db: PathBuf,
    #[arg(long)]
    user: Option<String>,
    #[arg(long, value_enum, default_value = "codex")]
    backend: Backend,
    #[arg(long)]
    model: Option<String>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Clone, ValueEnum)]
enum Backend {
    Codex,
    Openai,
}
#[derive(Subcommand)]
enum Commands {
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
    },
    #[command(about = "로컬 메일 보관함·편집기 열기")]
    Gui {
        #[arg(long, default_value = "8765")]
        port: u16,
        #[arg(long)]
        no_open: bool,
    },
}
fn run(args: Args) -> Result<()> {
    if let Commands::Gui { port, no_open } = args.command {
        return server::serve(&args.db, port, !no_open);
    }
    let user = args
        .user
        .context("--user로 본인 이메일 주소를 지정하세요.")?;
    if let Commands::Context { to, purpose } = args.command {
        let db = db::open(&args.db, true)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&gui::draft_context(&db, &user, &to, &purpose)?)?
        );
        return Ok(());
    }
    let mut llm: Box<dyn LanguageModel> = match args.backend {
        Backend::Codex => Box::new(Codex::new(
            args.model.or_else(|| std::env::var("CODEX_MODEL").ok()),
        )?),
        Backend::Openai => Box::new(OpenAI::new(
            std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            args.model
                .or_else(|| std::env::var("OPENAI_MODEL").ok())
                .unwrap_or_default(),
        )?),
    };
    let result = match args.command {
        Commands::Ingest {
            paths,
            alias,
            since,
        } => {
            for path in &paths {
                if !path.exists() {
                    bail!("경로가 없습니다: {}", path.display());
                }
            }
            let files = paths.into_iter().flat_map(|path| {
                if path.is_dir() {
                    Box::new(
                        walkdir::WalkDir::new(path)
                            .sort_by_file_name()
                            .into_iter()
                            .filter_map(|entry| match entry {
                                Ok(entry)
                                    if entry.file_type().is_file()
                                        && entry
                                            .path()
                                            .extension()
                                            .is_some_and(|s| s.eq_ignore_ascii_case("eml")) =>
                                {
                                    Some(Ok(entry.into_path()))
                                }
                                Ok(_) => None,
                                Err(error) => Some(Err(error.into())),
                            }),
                    ) as Box<dyn Iterator<Item = Result<PathBuf>>>
                } else {
                    Box::new(std::iter::once(Ok(path)))
                }
            });
            let mut files = files.peekable();
            if files.peek().is_none() {
                bail!("색인할 .eml 파일이 없습니다.");
            }
            let mut db = db::connect(&args.db)?;
            pipeline::ingest(&mut db, llm.as_mut(), files, &user, &alias, since)?
        }
        Commands::Draft {
            eml,
            to,
            request,
            role,
            relationship,
            limit,
        } => {
            let raw = if eml.as_os_str() == "-" {
                mail::read_limited(std::io::stdin().lock(), mail::MAX_MAIL_BYTES)?
            } else {
                mail::read_source(&eml)?
            };
            let db = db::connect(&args.db)?;
            pipeline::draft(
                &db,
                llm.as_mut(),
                &raw,
                pipeline::DraftOptions {
                    owner: &user,
                    recipient: &to,
                    request: &request,
                    role: role.as_deref(),
                    relationship: relationship.as_deref(),
                    limit: limit as usize,
                },
            )?
        }
        _ => unreachable!(),
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
fn main() {
    if let Err(error) = run(Args::parse()) {
        eprintln!("오류: {error:#}");
        std::process::exit(1);
    }
}
