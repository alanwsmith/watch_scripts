#![allow(unused)]
use anyhow::Result;
use clap::{arg, command};
use itertools::Itertools;
use permissions::is_executable;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
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
        let payload = Payload {
            initial_dir,
            raw_then_path,
            start_instant: None,
        };
        payload.validate_paths();
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

    pub fn validate_paths(&self) {
        if let None = &self.initial_dir {
            eprintln!("ERROR: getting current direction. Can not continue");
            std::process::exit(1);
        }

        if let Some(then_path) = &self.raw_then_path {
            if !then_path.exists() {
                eprintln!("ERROR: {} does not exist", then_path.display());
                std::process::exit(1);
            }
        }
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
            let paths_to_run = get_paths(&action.events);
            dbg!(paths_to_run);
            // for event in action.events.iter() {
            //     eprintln!("EVENT: {0:?}", event.tags);
            // }

            // clearscreen::clear().unwrap();
            if action.signals().any(|sig| sig == Signal::Interrupt) {
                action.quit(); // Needed for Ctrl+c
            } else {
                action.list_jobs().for_each(|(_, job)| {
                    job.delete_now();
                });
                let mut payload = payload.clone();
                let mut then_job_local: Option<Job> = None;
                if let Some(then_job) = payload.then_job() {
                    let (_, tmp_job) = action.create_job(then_job);
                    then_job_local = Some(tmp_job);
                }

                //let _ = payload.file_cd();
                //let (_, job) = action.create_job(payload.file_job());

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

// fn get_paths(events: &Arc<[Event]>) -> Vec<PathBuf> {
//     let extensions = ["rs", "py", "bash"];
//     events
//         .iter()
//         .filter(|event| {
//             event
//                 .tags
//                 .iter()
//                 .find(|tag| {
//                     if let Tag::FileEventKind(kind) = &tag {
//                         if let FileEventKind::Modify(mod_kind) = kind {
//                             if let ModifyKind::Data(change) = mod_kind {
//                                 if let DataChange::Content = change {
//                                     return true;
//                                 }
//                             }
//                         }
//                     };
//                     false
//                 })
//                 .is_some()
//         })
//         .filter_map(|event| {
//             event.tags.iter().find_map(|tag| {
//                 if let Tag::Path { path, .. } = tag {
//                     if let Some(file_name_path) = path.file_name() {
//                         let file_name = file_name_path.display().to_string();
//                         if file_name.starts_with(".") {
//                             return None;
//                         }
//                         if file_name.ends_with("~") {
//                             return None;
//                         }
//                         if let Some(ext) = path.extension() {
//                             if !extensions.contains(&ext.display().to_string().as_str()) {
//                                 return None;
//                             }
//                         }
//                     }
//                     Some(path.to_path_buf())
//                 } else {
//                     None
//                 }
//             })
//         })
//         .unique()
//         .collect()
// }

fn get_paths(events: &Arc<[Event]>) -> Vec<PathBuf> {
    events
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
        .unique()
        .collect()
}
