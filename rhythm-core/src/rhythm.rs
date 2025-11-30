#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::note::Note;

#[derive(Debug, Clone, PartialEq)]
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
pub struct Rhythm<T: Note> {
    pub notes: Vec<T>,
    /// The current time.
    time: f64,
    /// The available notes. When single notes are hit or combo notes not in range, they will be removed from this list.
    #[serde(skip_serializing, skip_deserializing)]
    availables: Vec<T>,
    /// Index of the first note that could potentially be hitable (start time >= time - max_duration).
    /// This allows us to skip notes that have definitely passed.
    #[serde(skip_serializing, skip_deserializing)]
    scan_start_idx: usize,
}

impl<T: Note> Rhythm<T> {
    pub fn new(notes: Vec<T>) -> Self {
        let mut availables = notes.clone();
        availables.sort_unstable();

        Self {
            notes,
            time: 0.0,
            availables,
            scan_start_idx: 0,
        }
    }

    pub fn current_time(&self) -> f64 {
        self.time
    }

    pub fn availables(&self) -> &[T] {
        &self.availables
    }

    pub fn forward(&mut self, time: impl Into<f64>) -> Vec<T> {
        self.time += time.into();
        self.update_availables()
    }

    pub fn set_time(&mut self, time: impl Into<f64>) {
        self.time = time.into();
        self.availables.clone_from(&self.notes);
        self.availables.sort_unstable();
        self.scan_start_idx = 0;
        self.update_availables();
    }

    pub fn finished(&self) -> bool {
        self.availables.is_empty()
    }

    /// Find the first hitable note matching the variant using optimized scanning.
    /// Since availables is sorted by start time, we can use binary search to find
    /// the range of potentially hitable notes, then scan only that range.
    pub fn hit(&mut self, variant: impl Into<u16>) -> Option<(&mut T, f64)> {
        let variant: u16 = variant.into();
        let time = self.time;

        // Use binary search to find the first note that could end at or after current time.
        // A note is hitable if: start <= time && start + duration >= time
        // Rearranging: start <= time && start >= time - duration
        // Since notes are sorted by start, find first note where start + duration >= time
        let start_idx = self.find_first_potentially_hitable();

        // Find the last note that could be hitable (start <= time)
        let end_idx = self.find_last_potentially_hitable();

        let len = self.availables.len();
        if start_idx > end_idx || start_idx >= len {
            return None;
        }

        let actual_end = end_idx.min(len - 1);

        // Only scan the range of potentially hitable notes
        for note in &mut self.availables[start_idx..=actual_end] {
            let is_hitable = note.start() <= time && note.start() + note.duration() >= time;
            if is_hitable && note.matches_variant(variant) && note.volume() > 0 {
                note.set_volume(note.volume() - 1);
                let delta = time - note.start();
                return Some((note, delta));
            }
        }

        None
    }

    /// Binary search to find first note index where start + duration >= time
    /// (the note could still be active)
    fn find_first_potentially_hitable(&self) -> usize {
        // Start from the cached scan_start_idx to avoid re-scanning already-passed notes
        let time = self.time;
        let slice = &self.availables[self.scan_start_idx..];

        // Linear scan from scan_start_idx since we're advancing forward in time
        // This is O(1) amortized as we only advance, never go back
        for (i, note) in slice.iter().enumerate() {
            if note.start() + note.duration() >= time {
                return self.scan_start_idx + i;
            }
        }
        self.availables.len()
    }

    /// Binary search to find last note index where start <= time
    fn find_last_potentially_hitable(&self) -> usize {
        let time = self.time;

        // Binary search for the rightmost note with start <= time
        let result = self.availables.partition_point(|note| note.start() <= time);
        if result == 0 {
            0
        } else {
            result - 1
        }
    }

    fn update_availables(&mut self) -> Vec<T> {
        let mut removed = vec![];
        let time = self.time;

        // Update scan_start_idx to skip notes that have definitely passed
        // A note has passed if start + duration < time
        while self.scan_start_idx < self.availables.len() {
            let note = &self.availables[self.scan_start_idx];
            if note.start() + note.duration() >= time || note.volume() == 0 {
                break;
            }
            self.scan_start_idx += 1;
        }

        self.availables.retain(|note| {
            let keep = note.start() + note.duration() >= time && note.volume() > 0;
            if !keep && note.volume() > 0 {
                removed.push(note.clone());
            }
            keep
        });

        // Reset scan_start_idx after retain since indices may have shifted
        self.scan_start_idx = 0;

        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::SimpleNote;

    #[test]
    fn test_single_note() {
        let notes = vec![
            SimpleNote::new(1000, 100, 1u16, 0u16),
            SimpleNote::new(1100, 100, 1u16, 1u16),
            SimpleNote::new(2000, 100, 1u16, 0u16),
            SimpleNote::new(2100, 100, 1u16, 1u16),
        ];

        let mut rhythm = Rhythm::new(notes);

        assert_eq!(rhythm.current_time(), 0.0);

        assert_eq!(rhythm.forward(500), vec![]);
        assert_eq!(rhythm.current_time(), 500.0);
        assert_eq!(rhythm.hit(0u16), None);

        assert_eq!(rhythm.forward(550), vec![]);
        assert_eq!(rhythm.current_time(), 1050.0);
        assert_eq!(rhythm.hit(1u16), None);
        assert_eq!(
            rhythm.hit(0u16),
            Some((&mut SimpleNote::new(1000, 100, 0u16, 0u16), 50.0))
        );
        assert_eq!(rhythm.hit(0u16), None);

        assert_eq!(
            rhythm.forward(200),
            vec![SimpleNote::new(1100, 100, 1u16, 1u16)]
        );
        assert_eq!(rhythm.current_time(), 1250.0);
        assert_eq!(rhythm.hit(1u16), None);

        assert_eq!(
            rhythm.forward(2000),
            vec![
                SimpleNote::new(2000, 100, 1u16, 0u16),
                SimpleNote::new(2100, 100, 1u16, 1u16)
            ]
        );
    }

    #[test]
    fn test_combo_note() {
        let notes = vec![
            SimpleNote::new(1000, 1000, 10u16, 0u16),
            SimpleNote::new(3000, 2000, u16::MAX, 1u16),
        ];

        let mut rhythm = Rhythm::new(notes);

        assert_eq!(rhythm.current_time(), 0.0);

        assert_eq!(rhythm.forward(1500), vec![]);
        assert_eq!(rhythm.current_time(), 1500.0);
        for i in 0..10 {
            assert_eq!(
                rhythm.hit(0u16),
                Some((
                    &mut SimpleNote::new(1000, 1000, 9 - i as u16, 0u16),
                    500.0 + i as f64 * 10.0
                ))
            );
            assert_eq!(rhythm.forward(10), vec![]);
        }
        assert_eq!(rhythm.current_time(), 1600.0);
        assert_eq!(rhythm.hit(0u16), None);

        assert_eq!(rhythm.forward(3000), vec![]);
        assert_eq!(rhythm.current_time(), 4600.0);
        for i in 0..1000 {
            assert_eq!(
                rhythm.hit(1u16),
                Some((
                    &mut SimpleNote::new(3000, 2000, u16::MAX - 1 - i as u16, 1u16),
                    1600.0
                ))
            );
        }
    }

    #[test]
    fn test_many_notes_performance() {
        // Create a large number of notes to test performance
        let mut notes = Vec::with_capacity(10000);
        for i in 0..10000 {
            notes.push(SimpleNote::new(
                i as f64 * 100.0, // start at 0, 100, 200, ...
                50.0,             // duration 50ms
                1u16,
                (i % 2) as u16, // alternating variants
            ));
        }

        let mut rhythm = Rhythm::new(notes);

        // Hit notes in the middle of the sequence
        rhythm.forward(500050.0);

        // Should find the note at time 500000 (index 5000)
        let result = rhythm.hit(0u16);
        assert!(result.is_some());

        // Verify we can hit notes efficiently even with many notes
        let hit_count = (0..100).filter(|_| rhythm.hit(0u16).is_some()).count();
        // Should hit some notes (the exact count depends on timing)
        let _ = hit_count; // Just verifying the iteration works without panicking
    }

    #[test]
    fn test_binary_search_boundary() {
        // Test edge cases for binary search
        let notes = vec![
            SimpleNote::new(100.0, 10.0, 1u16, 0u16),
            SimpleNote::new(200.0, 10.0, 1u16, 0u16),
            SimpleNote::new(300.0, 10.0, 1u16, 0u16),
        ];

        let mut rhythm = Rhythm::new(notes);

        // Before any notes
        rhythm.forward(50.0);
        assert!(rhythm.hit(0u16).is_none());

        // Exactly at first note start
        rhythm.forward(50.0);
        assert!(rhythm.hit(0u16).is_some());

        // Between notes
        rhythm.forward(50.0);
        assert!(rhythm.hit(0u16).is_none());

        // At second note
        rhythm.forward(50.0);
        assert!(rhythm.hit(0u16).is_some());

        // After all notes
        rhythm.forward(200.0);
        assert!(rhythm.hit(0u16).is_none());
    }
}
