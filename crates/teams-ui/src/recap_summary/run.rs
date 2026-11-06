//! One summary being made, on a thread of its own so the window never waits on the tool: a short transcript read whole
//! by one run of the tool; a long one condensed section by section (a run each, so no conversation grows past one
//! section) and summarized from the sections' notes by a last run. Dropping it ([`Summarizing`]) stops it and ends the
//! tool's process. Nothing here logs the transcript, the notes or the summary.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::{
    FIX_REQUEST, Input, Line, Made, instructions, notes_instructions, parse, sections, stamp, transcript_text,
};
use crate::local_ai::{Backend, Failure, Launch, Model, Tool, Work, cap};

/// The longest notes kept of one section.
const MAX_NOTES: usize = 12_000;

/// How much of the transcript goes in one pass, how long the tool may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most characters of transcript read in one pass; a longer transcript is condensed by sections.
    pub single: usize,
    /// The most characters of transcript in one section.
    pub section: usize,
    /// How long the tool may take to start.
    pub start: Duration,
    /// How long one answer may take.
    pub answer: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            single: 180_000,
            section: 90_000,
            start: Duration::from_secs(60),
            answer: Duration::from_secs(300),
        }
    }
}

/// How far a summary got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The transcript read whole.
    Summarizing,
    /// A long transcript's section `section` (from 1) of `of` condensed.
    Condensing { section: usize, of: usize },
    /// The summary written from the sections' notes.
    Writing,
}

impl Stage {
    /// What the recap says while it runs.
    #[must_use]
    pub fn words(self) -> String {
        match self {
            Self::Summarizing => "Reading the transcript\u{2026}".to_owned(),
            Self::Condensing { section, of } => format!("Reading part {section} of {of} of the transcript\u{2026}"),
            Self::Writing => "Writing the summary\u{2026}".to_owned(),
        }
    }
}

/// What a summary being made tells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    Stage(Stage),
    Done(Made),
    Failed(Failure),
}

/// What to summarize, with which tool and model.
#[derive(Clone, Debug)]
pub struct Request {
    pub backend: Backend,
    pub program: PathBuf,
    pub model: Model,
    pub input: Input,
    pub limits: Limits,
}

/// A summary being made: dropping it stops its thread and its tool.
pub struct Summarizing {
    stop: Arc<AtomicBool>,
    updates: async_channel::Receiver<Update>,
}

impl Summarizing {
    /// Starts making `request`'s summary on a thread of its own.
    #[must_use]
    pub fn start(request: Request) -> Self {
        let (out, updates) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let backend = request.backend;
        let worker = Worker {
            request,
            out: out.clone(),
            stop: Arc::clone(&stop),
        };
        if let Err(error) = thread::Builder::new()
            .name("recap-summary".into())
            .spawn(move || worker.run())
        {
            let _ = out.try_send(Update::Failed(Failure::Stopped(backend, Some(error.to_string()))));
        }
        Self { stop, updates }
    }

    /// Where its updates come.
    #[must_use]
    pub fn updates(&self) -> async_channel::Receiver<Update> {
        self.updates.clone()
    }
}

impl Drop for Summarizing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Worker {
    request: Request,
    out: async_channel::Sender<Update>,
    stop: Arc<AtomicBool>,
}

impl Worker {
    fn run(self) {
        let made = self.summarize();
        if self.stop.load(Ordering::Relaxed) {
            return;
        }
        let _ = self.out.try_send(match made {
            Ok(made) => Update::Done(made),
            Err(failure) => Update::Failed(failure),
        });
    }

    fn tell(&self, stage: Stage) {
        let _ = self.out.try_send(Update::Stage(stage));
    }

    fn summarize(&self) -> Result<Made, Failure> {
        let Request { input, limits, .. } = &self.request;
        let text = transcript_text(&input.lines);
        let (material, count) = if text.len() <= limits.single {
            self.tell(Stage::Summarizing);
            (format!("<transcript>\n{text}</transcript>"), 1)
        } else {
            let parts = sections(&input.lines, limits.section);
            let of = parts.len();
            let mut notes = String::new();
            for (index, part) in parts.iter().enumerate() {
                self.tell(Stage::Condensing { section: index + 1, of });
                let said = self.condense(part)?;
                let (from, to) = (
                    part.first().map_or(0, |line| line.at),
                    part.last().map_or(0, |line| line.at),
                );
                let _ = write!(
                    notes,
                    "<notes section=\"{}\" from=\"{}\" to=\"{}\">\n{}\n</notes>\n",
                    index + 1,
                    stamp(from),
                    stamp(to),
                    cap(&said, MAX_NOTES)
                );
            }
            self.tell(Stage::Writing);
            (notes, of)
        };
        let message = format!("<meeting>\n{}</meeting>\n{material}", input.brief);
        let backend = self.request.backend;
        let summary = self.with_tool(instructions(), |tool, wait, stop| {
            let end = input.end();
            if let Some(summary) = parse(&tool.ask(&message, wait, stop)?, end) {
                return Ok(summary);
            }
            parse(&tool.ask(FIX_REQUEST, wait, stop)?, end)
                .ok_or_else(|| Failure::Stopped(backend, Some("its answer wasn't a summary. Try again.".to_owned())))
        })?;
        Ok(Made {
            summary,
            tool: self.request.backend.key().to_owned(),
            model: self.request.model.clone(),
            made_at: chrono::Utc::now().timestamp_millis(),
            sections: count,
        })
    }

    /// One section's notes.
    fn condense(&self, part: &[Line]) -> Result<String, Failure> {
        let message = format!(
            "<meeting>\n{}</meeting>\n<section>\n{}</section>",
            self.request.input.brief,
            transcript_text(part)
        );
        self.with_tool(notes_instructions(), |tool, wait, stop| tool.ask(&message, wait, stop))
    }

    /// Starts the tool with `instructions`, runs `work` with it and ends it, whatever `work` came to.
    fn with_tool<T>(
        &self,
        instructions: String,
        work: impl FnOnce(&mut Tool, Duration, &AtomicBool) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let launch = Launch {
            backend: self.request.backend,
            program: self.request.program.clone(),
            work: Work::Summary,
            model: self.request.model.clone(),
            instructions,
            first: self.request.limits.start,
        };
        let mut tool = Tool::start(&launch, &self.stop)?;
        // the first answer also waits for the tool to start
        let wait = self.request.limits.answer + self.request.limits.start;
        let done = work(&mut tool, wait, &self.stop);
        tool.close();
        done
    }
}
