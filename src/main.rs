mod config;

use anyhow::{Context, Result, bail};
use config::{Args, Backend, Commands, GmailCommand};
use mimicmail::{
    db, drafting, gmail,
    llm::{Codex, LanguageModel, OpenAI},
    mail, pipeline, server,
};
use std::path::PathBuf;

fn run(args: Args) -> Result<()> {
    let make_model = {
        let backend = args.backend.clone();
        let model = args.model.clone();
        let api_key = args.api_key.clone();
        move || -> Result<Box<dyn LanguageModel>> {
            Ok(match backend {
                Backend::Codex => Box::new(Codex::new(model.clone())?),
                Backend::Openai => Box::new(OpenAI::new(
                    api_key.clone(),
                    model.clone().unwrap_or_default(),
                )?),
            })
        }
    };
    if let Commands::Gui { port, no_open } = args.command {
        let _collector = if args.gmail_enabled {
            args.gmail.as_ref().unwrap().validate()?;
            db::connect(&args.db)?;
            Some(gmail::Background::start(
                args.db.clone(),
                args.gmail.unwrap(),
                make_model,
            )?)
        } else {
            None
        };
        return server::serve(&args.db, port, !no_open);
    }
    if let Commands::Gmail { command } = args.command {
        let settings = args.gmail.unwrap();
        match command {
            GmailCommand::Connect { no_open } => gmail::connect(&settings, &args.db, !no_open)?,
            GmailCommand::Sync { retry_failed } => println!(
                "{}",
                serde_json::to_string_pretty(&gmail::synchronize(
                    &args.db,
                    &settings,
                    &mimicmail::work::Work::default(),
                    &make_model,
                    retry_failed
                )?)?
            ),
            GmailCommand::Status => println!(
                "{}",
                serde_json::to_string_pretty(&gmail::status(&db::open(&args.db, true)?)?)?
            ),
            GmailCommand::Watch => {
                let _collector = gmail::Background::start(args.db, settings, make_model)?;
                tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
                    #[cfg(unix)]
                    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
                    tokio::select! {
                        result = tokio::signal::ctrl_c() => result?,
                        _ = async { #[cfg(unix)] { terminate.recv().await; } #[cfg(not(unix))] { std::future::pending::<()>().await; } } => {},
                    }
                    Ok::<(),anyhow::Error>(())
                })?;
            }
        }
        return Ok(());
    }
    let user = args
        .user
        .context("--user로 본인 이메일 주소를 지정하세요.")?;
    if let Commands::Context {
        to,
        purpose,
        history_key,
        style_id,
    } = args.command
    {
        let db = db::open(&args.db, true)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&drafting::selected_context(
                &db,
                &user,
                &to,
                &purpose,
                &history_key,
                &style_id
            )?)?
        );
        return Ok(());
    }
    let mut llm = make_model()?;
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
    if let Err(error) = Args::load().and_then(run) {
        eprintln!("오류: {error:#}");
        std::process::exit(1);
    }
}
