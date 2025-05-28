#![allow(unused)]
use anyhow::Result;
use clap::{arg, command};
use itertools::Itertools;
use permissions::is_executable;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use watchexec::Id;
use watchexec::WatchedPath;
use watchexec::Watchexec;
use watchexec::command::Command as WatchCommand;
use watchexec::command::Program;
use watchexec::command::Shell;
use watchexec::job::Job;
use watchexec_events::Event;
use watchexec_events::Tag;
use watchexec_events::filekind::DataChange;
use watchexec_events::filekind::FileEventKind;
use watchexec_events::filekind::ModifyKind;
use watchexec_signals::Signal;

#[derive(Debug, Clone)]
struct Payload {
    initial_dir: Option<PathBuf>,
    raw_then_path: Option<PathBuf>,
    start_instant: Option<Instant>,
}

impl Payload {
    // pub fn file_cd(&self) -> Result<()> {
    //     std::env::set_current_dir(self.initial_dir.as_ref().unwrap())?;
    //     if let Some(parent_dir) = self.raw_file_path.as_ref().unwrap().parent() {
    //         std::env::set_current_dir(parent_dir)?;
    //     }
    //     Ok(())
    // }

    // pub fn file_command(&self) -> String {
    //     format!(
    //         "./{}",
    //         self.raw_file_path
    //             .as_ref()
    //             .unwrap()
    //             .file_name()
    //             .unwrap()
    //             .display()
    //             .to_string()
    //     )
    // }

    // pub fn file_job(&self) -> Arc<WatchCommand> {
    //     Arc::new(WatchCommand {
    //         program: Program::Shell {
    //             shell: Shell::new("bash"),
    //             command: self.file_command(),
    //             args: vec![],
    //         },
    //         options: Default::default(),
    //     })
    // }

    pub fn get_args() -> Result<(Option<PathBuf>, bool, Option<PathBuf>)> {
        let matches = command!()
            .arg(
                arg!(
    -t --then <then_path>
                "Script to run after the main process is done")
                .value_parser(clap::value_parser!(PathBuf)),
            )
            .get_matches();
        Ok((
            matches.get_one::<PathBuf>("then").cloned(),
            false, //matches.get_flag("verbose"),
            std::env::current_dir().ok(),
        ))
    }

    pub fn mark_time(&mut self) {
        self.start_instant = Some(Instant::now());
    }

    pub fn new() -> Result<Payload> {
        let (raw_then_path, _verbose, initial_dir) = Payload::get_args()?;
        let mut payload = Payload {
            initial_dir,
            raw_then_path,
            start_instant: None,
        };
        payload.validate_paths()?;
        Ok(payload)
    }

    pub fn then_cd(&self) -> Result<()> {
        std::env::set_current_dir(self.initial_dir.as_ref().unwrap())?;
        if let Some(parent_dir) = self.raw_then_path.as_ref().unwrap().parent() {
            std::env::set_current_dir(parent_dir)?;
        }
        Ok(())
    }

    pub fn then_command(&self) -> Option<String> {
        if let Some(raw_then_path) = self.raw_then_path.as_ref() {
            Some(format!(
                "./{}",
                raw_then_path.file_name().unwrap().display().to_string()
            ))
        } else {
            None
        }
    }

    pub fn then_job(&self) -> Option<Arc<WatchCommand>> {
        if let Some(then_command) = self.then_command() {
            Some(Arc::new(WatchCommand {
                program: Program::Shell {
                    shell: Shell::new("bash"),
                    command: then_command,
                    args: vec![],
                },
                options: Default::default(),
            }))
        } else {
            None
        }
    }

    pub fn validate_paths(&mut self) -> Result<()> {
        if let None = &self.initial_dir {
            eprintln!("ERROR: could not get current directory. Can not continue.");
            std::process::exit(1);
        }
        if let Some(then_path) = &self.raw_then_path {
            if !then_path.exists() {
                eprintln!("ERROR: {} does not exist", then_path.display());
                std::process::exit(1);
            }
            match is_executable(then_path) {
                Ok(check) => {
                    if !check {
                        eprintln!("ERROR: {} is not executable", then_path.display());
                        std::process::exit(1);
                    }
                }
                Err(_) => {
                    eprintln!(
                        "ERROR: Could not determine permissions for {} ",
                        then_path.display()
                    );
                    std::process::exit(1);
                }
            }
            self.raw_then_path = Some(fs::canonicalize(then_path)?);
        }

        Ok(())
    }

    pub fn watch_path(&self) -> PathBuf {
        PathBuf::from(".")
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let payload = Payload::new()?;
    let runner = Runner::new(payload)?;
    runner.run().await?;
    Ok(())
}

struct Runner {
    payload: Payload,
}

impl Runner {
    pub fn new(payload: Payload) -> Result<Runner> {
        Ok(Runner { payload })
    }

    pub async fn run(&self) -> Result<()> {
        clearscreen::clear().unwrap();
        println!("Watching for script changes");
        if let Some(then_path) = self.payload.raw_then_path.as_ref() {
            println!("Then Running: {}", then_path.display());
        }
        let wx = Watchexec::default();
        let payload = self.payload.clone();
        let watch_path = WatchedPath::recursive(self.payload.watch_path());
        wx.config.pathset(vec![watch_path]);
        wx.config.on_action(move |mut action| {
            if action.signals().any(|sig| sig == Signal::Interrupt) {
                action.quit(); // Needed for Ctrl+c
            } else {
                if let Some(details) = get_command(&action.events, payload.raw_then_path.as_ref()) {
                    clearscreen::clear().unwrap();
                    if let Err(_) = std::env::set_current_dir(payload.initial_dir.as_ref().unwrap())
                    {
                        return action;
                    }
                    if let Some(cd_to) = details.clone().0 {
                        if let Err(_) = std::env::set_current_dir(cd_to) {
                            return action;
                        }
                    }
                    action.list_jobs().for_each(|(_, job)| {
                        job.delete_now();
                    });
                    let (id, job) = action.create_job(details.clone().1);
                    job.start();
                    // details.2 is the check for if then_path is the same path
                    if details.2 {
                        if let Some(then_job) = payload.then_job() {
                            let payload = payload.clone();
                            let (_, then_run) = action.create_job(then_job);
                            tokio::spawn(async move {
                                job.to_wait().await;
                                if !job.is_dead() {
                                    job.run(move |jtc| {
                                        if let watchexec::job::CommandState::Finished {
                                            status,
                                            started,
                                            finished,
                                        } = jtc.current
                                        {
                                            if let watchexec_events::ProcessEnd::Success = status {
                                                if let Ok(_) = payload.then_cd() {
                                                    then_run.start();
                                                }
                                            }
                                        }
                                    });
                                }
                            });
                        }
                    }
                }

                // let paths_to_run = get_paths(&action.events);
                // dbg!(paths_to_run);
                // for event in action.events.iter() {
                //     eprintln!("EVENT: {0:?}", event.tags);
                // }
                //
                //

                // action.list_jobs().for_each(|(_, job)| {
                //     job.delete_now();
                // });

                // let mut payload = payload.clone();
                // let mut then_job_local: Option<Job> = None;
                // if let Some(then_job) = payload.then_job() {
                //     let (_, tmp_job) = action.create_job(then_job);
                //     then_job_local = Some(tmp_job);
                // }

                //let (_, job) = action.create_job(payload.file_job());
                //let _ = payload.file_cd();

                // payload.mark_time();
                // job.start();

                // tokio::spawn(async move {
                //     job.to_wait().await;
                //     if !job.is_dead() {
                //         // payload.print_report();
                //         if let Some(then_job_runner) = then_job_local {
                //             let _ = payload.then_cd();
                //             then_job_runner.start();
                //         }
                //     }
                // });
            }
            action
        });
        let _ = wx.main().await?;
        Ok(())
    }
}

// the bool is if the matched path is the same of the then
// path in which case the script shouldn't be run twice.
// not the greatest was to do this check, but works for now
fn get_command(
    events: &Arc<[Event]>,
    then_path: Option<&PathBuf>,
) -> Option<(Option<PathBuf>, Arc<WatchCommand>, bool)> {
    if let Some(p) = events
        .iter()
        .filter(|event| {
            event
                .tags
                .iter()
                .find(|tag| {
                    if let Tag::FileEventKind(kind) = &tag {
                        if let FileEventKind::Modify(mod_kind) = kind {
                            if let ModifyKind::Data(change) = mod_kind {
                                if let DataChange::Content = change {
                                    return true;
                                }
                            }
                        }
                    };
                    false
                })
                .is_some()
        })
        .filter_map(|event| {
            event.tags.iter().find_map(|tag| {
                if let Tag::Path { path, .. } = tag {
                    match is_executable(path) {
                        Ok(check) => {
                            if !check {
                                return None;
                            }
                        }
                        _ => {
                            return None;
                        }
                    }
                    for component in path.components() {
                        if let std::path::Component::Normal(part) = component {
                            if part.display().to_string().starts_with(".") {
                                return None;
                            }
                        }
                    }
                    if let Some(file_name_path) = path.file_name() {
                        let file_name = file_name_path.display().to_string();
                        if file_name.ends_with("~") {
                            return None;
                        }
                    };
                    Some(path.to_path_buf())
                } else {
                    None
                }
            })
        })
        .nth(0)
    {
        let full_path = fs::canonicalize(&p).unwrap();
        let run_then = match then_path {
            Some(p) => *p != full_path,
            None => false,
        };
        let cd_to = match p.parent() {
            Some(p_dir) => Some(p_dir.to_path_buf()),
            None => None,
        };
        let file_to_run = p.file_name()?;
        Some((
            cd_to,
            Arc::new(WatchCommand {
                program: Program::Shell {
                    shell: Shell::new("bash"),
                    command: format!("./{}", file_to_run.to_string_lossy().to_string()),
                    args: vec![],
                },
                options: Default::default(),
            }),
            run_then,
        ))
    } else {
        None
    }
}
