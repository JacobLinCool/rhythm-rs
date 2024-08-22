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
}

impl<T: Note> Rhythm<T> {
    pub fn new(notes: Vec<T>) -> Self {
        let mut availables = notes.clone();
        availables.sort_unstable();

        Self {
            notes,
            time: 0.0,
            availables,
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
        self.update_availables();
    }

    pub fn finished(&self) -> bool {
        self.availables.is_empty()
    }

    pub fn hit(&mut self, variant: impl Into<u16>) -> Option<(&mut T, f64)> {
        let variant: u16 = variant.into();
        let hitables = self.availables.iter_mut().filter(|note| {
            note.start() <= self.time && note.start() + note.duration() >= self.time
        });

        for note in hitables {
            if note.matches_variant(variant) && note.volume() > 0 {
                note.set_volume(note.volume() - 1);
                let time = self.time - note.start();
                return Some((note, time));
            }
        }

        None
    }

    fn update_availables(&mut self) -> Vec<T> {
        let mut removed = vec![];
        self.availables.retain(|note| {
            let keep = note.start() + note.duration() >= self.time && note.volume() > 0;
            if !keep && note.volume() > 0 {
                removed.push(note.clone());
            }
            keep
        });
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
}
