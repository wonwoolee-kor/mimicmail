mod config;

use anyhow::{Context, Result, bail};
use config::{Args, Backend, Commands};
use mimicmail::{
    db, drafting,
    llm::{Codex, LanguageModel, OpenAI},
    mail, pipeline, server,
};
use std::path::PathBuf;

fn run(args: Args) -> Result<()> {
    if let Commands::Gui { port, no_open } = args.command {
        return server::serve(&args.db, port, !no_open);
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
    let mut llm: Box<dyn LanguageModel> = match args.backend {
        Backend::Codex => Box::new(Codex::new(args.model)?),
        Backend::Openai => Box::new(OpenAI::new(args.api_key, args.model.unwrap_or_default())?),
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
    if let Err(error) = Args::load().and_then(run) {
        eprintln!("오류: {error:#}");
        std::process::exit(1);
    }
}
