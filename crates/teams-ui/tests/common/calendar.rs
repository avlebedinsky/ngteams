//! A synthetic calendar service for view tests, including replies held across navigation or a save.

use std::collections::{HashMap, VecDeque};

use teams_core::{
    Answer, CalendarEvent, EngineError, EventWrite, EventWritten, FullEvent, Invitable, MeetingAvailability, Reply,
};

#[derive(Default)]
pub struct CalendarFake {
    pub events: Vec<CalendarEvent>,
    pub reads: Vec<(i64, i64)>,
    pub answers: VecDeque<Result<Vec<CalendarEvent>, EngineError>>,
    pub hold: bool,
    pub held: Vec<Answer<Vec<CalendarEvent>>>,
    pub full: HashMap<String, FullEvent>,
    pub full_reads: Vec<String>,
    pub writes: Vec<EventWrite>,
    pub write_answers: VecDeque<Result<EventWritten, EngineError>>,
    pub hold_writes: bool,
    pub held_writes: Vec<Answer<EventWritten>>,
    pub people: Vec<Invitable>,
    pub people_queries: Vec<String>,
    pub availability: MeetingAvailability,
}

impl CalendarFake {
    pub fn read(&mut self, start_ms: i64, end_ms: i64) -> Reply<Vec<CalendarEvent>> {
        self.reads.push((start_ms, end_ms));
        if self.hold {
            let (answer, reply) = Reply::later();
            self.held.push(answer);
            reply
        } else {
            Reply::ready(self.answers.pop_front().unwrap_or_else(|| Ok(self.events.clone())))
        }
    }

    pub fn write(&mut self, write: EventWrite) -> Reply<EventWritten> {
        let kind = write.kind();
        self.writes.push(write);
        if self.hold_writes {
            let (answer, reply) = Reply::later();
            self.held_writes.push(answer);
            reply
        } else {
            Reply::ready(self.write_answers.pop_front().unwrap_or_else(|| {
                Err(EngineError::EventNotSaved {
                    kind,
                    why: "nothing saves here".to_owned(),
                })
            }))
        }
    }
}
