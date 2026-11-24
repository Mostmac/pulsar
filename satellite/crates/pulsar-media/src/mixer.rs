//! Мастер-громкость: softvol «Master Playback Volume» на карте 0 (0..255, линейно в дБ, у Миди
//! 2 значения и −60 дБ..0 дБ; TLV mute=0, то есть raw 0 — низ шкалы, а не тишина) по штатной
//! кривой модели (`Model::volume_map`): громкость v — уровень платформы v × верхний уровень.
//! Регулятор — пользовательский контрол, который alsa-lib создаёт при первом открытии его PCM
//! (`Model::master_pcm`), поэтому [`Mixer::open`] создаёт его, если его нет.
//!
//! Кривые живут в `pulsar_core::volume` и повторяют арифметику C++ шаг за шагом (f32 / f64 / int,
//! как там): значения state.conf, записанные одним бинарём, читает другой (`.prev`).

use log::{debug, error, info};
use pulsar_alsa::{Ctl, Pcm, Stream};
use pulsar_core::model::Model;
use pulsar_core::volume::{DuckingMode, clamp_unit, from_raw, music_gain, to_raw};

const MASTER: &str = "Master Playback Volume";

/// Мастер-громкость. Без контрола (открыть не удалось, это в логе) `available()` ложно, и громкость
/// хранится только в памяти. У моделей с программной громкостью (`Model::software_volume`)
/// контрола нет: `available()` истинно, и контроллер применяет громкость как усиление каналов.
/// Не актор: `set_*` применяется синхронно, геттеры сразу видят новое значение.
pub struct Mixer {
    model: &'static Model,
    ctl: Option<Ctl>,
    volume: f32,
    muted: bool,
}

impl Mixer {
    /// Card 0; adopts the current raw value when it reads as > 0, else the volume stays 0.5 (0.1
    /// with software volume).
    pub fn open(model: &'static Model) -> Mixer {
        let mut mixer = Mixer {
            model,
            ctl: None,
            // No hardware master: the controller applies this gain to the player channels.
            volume: if model.software_volume { 0.1 } else { 0.5 },
            muted: false,
        };
        if model.software_volume {
            info!("mixer: software volume");
            return mixer;
        }
        let ctl = match Ctl::open("hw:0") {
            Ok(ctl) => ctl,
            Err(rc) => {
                error!("mixer: snd_ctl_open hw:0: {}", pulsar_alsa::strerror(rc));
                return mixer;
            }
        };
        if !ctl.mixer_exists(MASTER) {
            // Однократное открытие PCM со softvol создаёт регулятор.
            drop(Pcm::open(model.master_pcm, Stream::Playback, true));
            if !ctl.mixer_exists(MASTER) {
                error!("mixer: control '{MASTER}' not found");
                return mixer;
            }
        }
        if let Ok(raw) = ctl.read_integer(MASTER, 0) {
            if raw > 0 {
                mixer.volume = from_raw(model, raw);
            }
            info!("mixer: master raw={raw} -> volume {:.2}", mixer.volume);
        }
        mixer.ctl = Some(ctl);
        mixer
    }

    pub fn available(&self) -> bool {
        self.model.software_volume || self.ctl.is_some()
    }

    /// 0..1
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// 0..1, written to the control at once.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = clamp_unit(v);
        self.apply();
    }

    pub fn music_gain(&self, mode: Option<DuckingMode>, limit: u8) -> f32 {
        if self.muted {
            return 0.0;
        }
        music_gain(self.model, self.volume, mode, limit, |raw| {
            self.ctl.as_ref()?.mixer_db(MASTER, raw)
        })
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Mute = raw 0 (still -60 dB, not silence).
    pub fn set_muted(&mut self, m: bool) {
        self.muted = m;
        self.apply();
    }

    fn apply(&self) {
        let Some(ctl) = &self.ctl else { return };
        let raw = if self.muted {
            0
        } else {
            to_raw(self.model, self.volume)
        };
        match ctl.write_integers(MASTER, &[raw, raw]) {
            Err(rc) => error!("mixer: write {raw}: {}", pulsar_alsa::strerror(rc)),
            Ok(()) => debug!(
                "mixer: volume {:.2} muted={} -> raw {raw}",
                self.volume,
                i32::from(self.muted)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulsar_core::model::model_from_compatible;
    use pulsar_core::volume::{step_alice, step_level};

    fn midi() -> &'static Model {
        model_from_compatible(b"yandex,cucumber\0").unwrap()
    }

    #[test]
    fn music_gain_tracks_volume_mute_mode_and_end_of_ducking() {
        let mut mixer = Mixer {
            model: model_from_compatible(b"yandex,station-mini2").unwrap(),
            ctl: None,
            volume: 1.0,
            muted: false,
        };
        let absolute = Some(DuckingMode::Absolute);
        let high = mixer.music_gain(absolute, 30);
        assert_eq!(mixer.volume(), 1.0);
        mixer.set_volume(0.6);
        assert!(mixer.music_gain(absolute, 30) > high);
        assert_eq!(mixer.music_gain(absolute, 70), 1.0);
        assert_eq!(mixer.music_gain(Some(DuckingMode::Relative), 0), 0.15);
        mixer.set_muted(true);
        assert_eq!(mixer.music_gain(absolute, 30), 0.0);
        assert_eq!(mixer.music_gain(None, 30), 0.0);
        mixer.set_muted(false);
        assert_eq!(mixer.music_gain(None, 30), 1.0);
        assert_eq!(mixer.volume(), 0.6);
    }

    #[test]
    fn steps_from_a_level_move_exactly_one() {
        for c in [
            &b"yandex,cucumber\0"[..],
            b"yandex,plum\0",
            b"yandex,station-mini2\0",
            b"yandex,bergamot\0",
            b"yandex,station-midi\0",
        ] {
            let m = model_from_compatible(c).unwrap();
            for k in 0..=20 {
                let v = k as f32 / 20.0;
                assert_eq!(step_level(m, v, true), (k + 1).min(20) as f32 / 20.0);
                assert_eq!(step_level(m, v, false), (k - 1).max(0) as f32 / 20.0);
            }
        }
        for k in 0..=10 {
            let v = k as f32 / 10.0;
            assert_eq!(step_alice(v, true), (k + 1).min(10) as f32 / 10.0);
            assert_eq!(step_alice(v, false), (k - 1).max(0) as f32 / 10.0);
        }
    }

    #[test]
    fn steps_from_between_levels_go_to_the_next_level() {
        let m = midi();
        assert_eq!(step_level(m, 0.07, false), 0.05);
        assert_eq!(step_level(m, 0.07, true), 0.1);
        assert_eq!(step_level(m, 0.33, true), 0.35);
        assert_eq!(step_level(m, 0.33, false), 0.3);
        assert_eq!(step_alice(0.05, true), 0.1);
        assert_eq!(step_alice(0.15, false), 0.1);
        assert_eq!(step_alice(0.33, false), 0.3);
        // f32 drift right next to a level counts as that level.
        assert_eq!(step_level(m, 0.150_000_1, false), 0.1);
        assert_eq!(step_level(m, 0.149_999_9, true), 0.2);
        assert_eq!(step_alice(0.199_999_9, true), 0.3);
    }

    #[test]
    fn steps_down_reach_exact_silence() {
        let m = midi();
        let mut v = 0.15;
        for _ in 0..3 {
            v = step_level(m, v, false);
        }
        assert_eq!(v, 0.0);
        let mut v = 0.3;
        for _ in 0..3 {
            v = step_alice(v, false);
        }
        assert_eq!(v, 0.0);
        assert_eq!(step_level(m, 1.0, true), 1.0);
        assert_eq!(step_alice(0.0, false), 0.0);
    }
}
