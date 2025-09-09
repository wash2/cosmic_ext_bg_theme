use std::time::Duration;

use cosmic_bg_config::state::State;
use cosmic_config::cosmic_config_derive::CosmicConfigEntry;
use cosmic_config::{Config, ConfigGet, ConfigSet, CosmicConfigEntry};
use cosmic_settings_daemon::{ConfigProxy, CosmicSettingsDaemonProxy};
use cosmic_theme::{Theme, ThemeBuilder};
use fast_image_resize::images::Image;
use fast_image_resize::{IntoImageView, Resizer};
use futures::StreamExt;
use kmeans_colors::{Kmeans, Sort, get_kmeans};
use palette::color_difference::Wcag21RelativeContrast;
use palette::{Clamp, FromColor, IntoColor, Lab, Lch, Saturate, Srgb, SrgbLuma, Srgba};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};
use zbus::Connection;

const ID: &str = "cosmic.ext.BgTheme";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).is_some_and(|a| a.as_str() != "--no-daemon") {
        println!("Usage: cosmic-ext-bg-theme [OPTIONAL --no-daemon]");
        println!("--no-daemon will exit immediately after setting the theme");
        std::process::exit(1);
    }
    let fmt_layer = fmt::layer().with_target(false);
    let filter_layer =
        EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info")).unwrap();
    if let Ok(journal_layer) = tracing_journald::layer() {
        tracing_subscriber::registry().with(journal_layer).with(filter_layer).init();
    } else {
        tracing_subscriber::registry().with(fmt_layer).with(filter_layer).init();
    }

    log_panics::init();
    tracing::info!("Starting CosmicExtBgTheme");

    let config = State::state()?;
    let config_context = cosmic_bg_config::context()?;

    let mut state = match State::get_entry(&config) {
        Ok(entry) => entry,
        Err((errs, entry)) => {
            for err in errs {
                tracing::error!("Failed to get the current state: {}", err);
            }
            entry
        },
    };

    if std::env::args().nth(1).is_some_and(|a| a.as_str() == "--no-daemon") {}
    let mut prev_state = None;

    if let Err(err) = apply_state(prev_state.as_ref(), &state, true) {
        tracing::error!("Failed to apply the state: {}", err);
    }

    if let Err(err) = apply_state(prev_state.as_ref(), &state, false) {
        tracing::error!("Failed to apply the state: {}", err);
    }
    if std::env::args().nth(1).is_some_and(|a| a.as_str() == "--no-daemon") {
        std::process::exit(0);
    }

    let settings_proxy = connect_settings_daemon().await?;
    let (path, name) = settings_proxy.watch_state(cosmic_bg_config::NAME, State::version()).await?;
    let bg_state_proxy = ConfigProxy::builder(settings_proxy.as_ref().connection())
        .path(path)?
        .destination(name)?
        .build()
        .await?;

    prev_state = Some(state.clone());

    let mut fail_count = 0;
    loop {
        fail_count = match run(
            &mut prev_state,
            fail_count,
            &bg_state_proxy,
            &settings_proxy,
            &mut state,
            &config,
        )
        .await
        {
            Ok(fail_count) => fail_count,
            Err(err) => {
                tracing::error!("Failed to run the main loop: {}", err);
                fail_count += 1;
                fail_count
            },
        };

        let config_dur =
            cosmic_bg_config::Config::load(&config_context).map_or(Duration::MAX, |c| {
                c.backgrounds
                    .first()
                    .map_or(Duration::MAX, |b| Duration::from_secs(b.rotation_frequency))
            });
        let sleep = Duration::from_secs(2_u64.saturating_pow(fail_count)).min(config_dur);
        tokio::time::sleep(sleep).await;
    }
}

async fn run(
    prev_state: &mut Option<State>,
    mut fail_count: u32,
    bg_state_proxy: &ConfigProxy<'static>,
    settings_proxy: &CosmicSettingsDaemonProxy<'static>,
    state: &mut State,
    config: &Config,
) -> anyhow::Result<u32> {
    let mut changes = bg_state_proxy.receive_changed().await?;

    let mut ownership_change = settings_proxy.as_ref().receive_owner_changed().await?;

    loop {
        let c = tokio::select! {
            c = changes.next() => c,
            c = ownership_change.next() => {
                if c.is_none() {
                    // The settings daemon has exited
                    tracing::error!("The settings daemon has exited");
                    break;
                } else {
                    None
                }
            },
        };
        let c = match c {
            Some(c) => c,
            None => {
                tracing::error!("Failed to receive the changes");
                break;
            },
        };
        let Ok(args) = c.args() else {
            continue;
        };
        let (errors, keys) = state.update_keys(&config, &[args.key]);
        if keys.is_empty() {
            continue;
        }
        for err in errors {
            tracing::error!("Failed to update the state: {}", err);
        }

        fail_count = 0;

        if let Err(err) = apply_state(prev_state.as_ref(), &state, true) {
            tracing::error!("Failed to apply the state: {}", err);
        }
        if let Err(err) = apply_state(prev_state.as_ref(), &state, false) {
            tracing::error!("Failed to apply the state: {}", err);
        }
        *prev_state = Some(state.clone());
    }

    fail_count += 1;
    Ok(fail_count)
}

async fn load_conn() -> anyhow::Result<Connection> {
    for _ in 0..5 {
        match Connection::session().await {
            Ok(conn) => return Ok(conn),
            Err(e) => {
                tracing::error!("Failed to connect to the session bus: {}", e);
                std::thread::sleep(std::time::Duration::from_secs(1));
            },
        }
    }
    Err(anyhow::anyhow!("Failed to connect to the session bus"))
}

async fn connect_settings_daemon() -> anyhow::Result<CosmicSettingsDaemonProxy<'static>> {
    let conn = load_conn().await?;
    for _ in 0..5 {
        match CosmicSettingsDaemonProxy::builder(&conn).build().await {
            Ok(proxy) => return Ok(proxy),
            Err(e) => {
                tracing::error!("Failed to connect to the settings daemon: {}", e);
                std::thread::sleep(std::time::Duration::from_secs(1));
            },
        }
    }
    Err(anyhow::anyhow!("Failed to connect to the settings daemon"))
}

fn apply_state(prev_state: Option<&State>, state: &State, is_dark: bool) -> anyhow::Result<()> {
    let changed = prev_state
        .as_ref()
        .and_then(|prev| {
            state.wallpapers.iter().find(|(k, v)| {
                prev.wallpapers.iter().find(|p| &p.0 == k).map_or(true, |prev_v| prev_v.1 != *v)
            })
        })
        .or_else(|| state.wallpapers.first());
    let Some(w) = changed else {
        anyhow::bail!("No wallpapers found");
    };
    let cosmic_bg_config::Source::Path(path) = &w.1 else {
        anyhow::bail!("No wallpaper path");
    };

    let bg_config = cosmic_config::Config::new(ID, MyConfig::VERSION)
        .map(|c| match MyConfig::get_entry(&c) {
            Ok(entry) => entry,
            Err((errs, entry)) => {
                for err in errs {
                    tracing::error!("Failed to get the config: {}", err);
                }
                entry
            },
        })
        .unwrap_or_default();

    let p = format!("{}_{}", path.to_string_lossy().replace("/", "_"), is_dark);
    if use_saved_result(&p, is_dark).is_ok() {
        return Ok(());
    }

    let kmeans_p = format!("{}_kmeans", p);

    let kmeans_config = cosmic_config::Config::new_state(ID, 1);

    let mut res =
        match kmeans_config.as_ref().ok().and_then(|c| c.get::<KmeanState>(&kmeans_p).ok()) {
            Some(res) if !res.0.is_empty() => res.0,
            _ => {
                let img = image::ImageReader::open(path)?.with_guessed_format()?.decode()?;

                // resize to width == 256
                let dst_width = 256;
                let dst_height =
                    (dst_width as f32 / img.width() as f32 * img.height() as f32) as u32;
                let mut dst_image = Image::new(dst_width, dst_height, img.pixel_type().unwrap());
                let mut resizer = Resizer::new();
                resizer.resize(&img, &mut dst_image, None)?;

                let img: Vec<Lab> = dst_image
                    .into_vec()
                    .chunks(3)
                    .map(|p| {
                        let rgb = Srgb::<u8>::new(p[0], p[1], p[2]);
                        rgb.into_format().into_color()
                    })
                    .collect();

                let mut results = Vec::new();
                let seed = 42;
                // TODO elbow method
                let mut best_result = Kmeans::new();
                for i in 0..2 {
                    let run_result = get_kmeans(8, 40, 10., false, &img, seed + i as u64);
                    if run_result.score < best_result.score {
                        best_result = run_result;
                    }
                }
                results.push(best_result);

                let Some(kmeans) = results.into_iter().last() else {
                    anyhow::bail!("No kmeans result");
                };
                let centroids = kmeans.centroids.clone();
                let mut res = Lab::sort_indexed_colors(&centroids, &kmeans.indices);
                res.sort_unstable_by(|a, b| (b.percentage).total_cmp(&a.percentage));

                let mut res = res.into_iter().map(|c| c.centroid).collect::<Vec<Lab>>();
                // move avoid colors to the end
                let avoid = if is_dark { &bg_config.avoid_dark } else { &bg_config.avoid_light };
                let mut avoid_colors = Vec::new();
                res.retain(|c| {
                    if avoid.iter().any(|a| *a == (*c).into_color()) {
                        avoid_colors.push(*c);
                        false
                    } else {
                        true
                    }
                });
                res.extend(avoid_colors);

                // move low chroma colors to the end
                let mut low_chroma = Vec::new();
                res.retain(|c| {
                    let lch = Lch::from_color(*c);
                    if lch.chroma < 10. {
                        low_chroma.push(*c);
                        false
                    } else {
                        true
                    }
                });
                res.extend(low_chroma);

                if bg_config.save_kmeans {
                    if let Ok(kmeans_config) = kmeans_config {
                        if let Err(err) = kmeans_config.set(&kmeans_p, KmeanState(res.clone())) {
                            tracing::error!("Failed to save the kmeans result: {}", err);
                        }
                    }
                }

                res
            },
        };

    let (builder_config, default) = if is_dark {
        (ThemeBuilder::dark_config()?, Theme::dark_default())
    } else {
        (ThemeBuilder::light_config()?, Theme::light_default())
    };

    let mut t = match ThemeBuilder::get_entry(&builder_config) {
        Ok(entry) => entry,
        Err((errs, entry)) => {
            for err in errs {
                tracing::error!("Failed to get the dark theme: {}", err);
            }
            entry
        },
    };

    // BG
    let default_window_bg = Lch::from_color(default.background.base);

    for c in &res {
        // make sure not in avoid after adjusting
        let mut new_window_bg: Lch = (*c).into_color();
        if (new_window_bg.chroma - default_window_bg.chroma).abs() > 15. {
            new_window_bg.chroma = default_window_bg.chroma + 15.;
            new_window_bg = new_window_bg.clamp();
        }
        if is_dark
            && bg_config.avoid_dark.iter().any(|a| {
                let a = Lch::from_color(*a);
                let hue_diff = (new_window_bg.hue.into_inner() - a.hue.into_inner()).abs() % 180.;
                (new_window_bg.chroma - a.chroma).powf(2.) + (hue_diff).powf(2.) < 666.
                    && hue_diff < 20.
            })
            || !is_dark
                && bg_config.avoid_light.iter().any(|a| {
                    let a = Lch::from_color(*a);
                    let hue_diff =
                        (new_window_bg.hue.into_inner() - a.hue.into_inner()).abs() % 180.;
                    (new_window_bg.chroma - a.chroma).powf(2.) + (hue_diff).powf(2.) < 666.
                        && hue_diff < 20.
                })
        {
            continue;
        }

        new_window_bg.l = default_window_bg.l;

        t = t.bg_color(new_window_bg.into_color());

        res.retain(|c| {
            let c = Lch::from_color(*c);
            (c.hue - new_window_bg.hue).into_inner().abs() > 10.
        });
        break;
    }

    // ACCENT
    let avoid =
        if is_dark { &bg_config.avoid_accents_dark } else { &bg_config.avoid_accents_light };

    let accent_res =
        if bg_config.randomize { left_skewed_shuffle(res.clone(), Some(3)) } else { res.clone() };

    let mut accent: (Lab, Lch) = (accent_res[0], accent_res[0].into_color());
    let mut best = f32::MIN;
    for (i, color) in accent_res.iter().enumerate() {
        let lch_orig = Lch::from_color(*color);
        let adjusted = adjust_lightness_for_contrast(
            (*color).into_color(),
            default.background.base.into_color(),
            4.5,
        );
        let mut score = adjusted.chroma;
        if avoid.iter().any(|c| {
            let c = Lch::from_color(*c);
            let hue_diff = (adjusted.hue.into_inner() - c.hue.into_inner()).abs() % 180.;
            (adjusted.chroma - c.chroma).powf(2.) + (hue_diff).powf(2.) < 666. && hue_diff < 20.
        }) {
            score /= 10.;
        } else if lch_orig.chroma > 60. && i <= res.len() / 3 {
            accent = (*color, adjusted);
            break;
        }
        if score > best {
            best = score;
            accent = (*color, adjusted);
        }
    }
    let max_hue_diff = res
        .iter()
        .map(|c| {
            let c = Lch::from_color(*c);
            (c.hue - accent.1.hue).into_inner().abs()
        })
        .max_by(|a, b| a.total_cmp(b))
        .unwrap();

    res.retain(|c| {
        let c = Lch::from_color(*c);
        (c.hue - accent.1.hue).into_inner().abs() > max_hue_diff / 6.
    });

    let accent = Srgb::from_color(accent.1);
    t = t.accent(accent);

    let mut res = if bg_config.randomize { left_skewed_shuffle(res, None) } else { res };

    // NEUTRAL
    let mut neutral = default.palette.neutral_5;

    for c in &res {
        let c_lch = Lch::from_color(*c);
        if c_lch.chroma > 10. {
            neutral = c_lch.into_color();
            break;
        }
    }

    t = t.neutral_tint(neutral.into_color());

    // TEXT
    if !res.is_empty() {
        t = t.text_tint(res.remove(0).into_color());
    };

    let result = BgResult {
        accent,
        bg: t.bg_color.unwrap(),
        neutral: t.neutral_tint.unwrap(),
        text: t.text_tint.map(|c| c.into_color()),
    };
    if bg_config.save_results {
        let my_config = cosmic_config::Config::new_state(ID, 1)?;
        if let Err(err) = my_config.set(&p, result) {
            tracing::error!("Failed to save the result: {}", err);
        }
    }

    // PALETTE
    // match chroma and lightness to accent for all palette colors
    let blue = t.palette.as_mut().accent_blue;
    t.palette.as_mut().accent_blue = sync_chroma_lightness(accent, blue);

    let green = t.palette.as_mut().accent_green;
    t.palette.as_mut().accent_green = sync_chroma_lightness(accent, green);

    let orange = t.palette.as_mut().accent_orange;
    t.palette.as_mut().accent_orange = sync_chroma_lightness(accent, orange);

    let purple = t.palette.as_mut().accent_purple;
    t.palette.as_mut().accent_purple = sync_chroma_lightness(accent, purple);

    let red = t.palette.as_mut().accent_red;
    t.palette.as_mut().accent_red = sync_chroma_lightness(accent, red);

    let yellow = t.palette.as_mut().accent_yellow;
    t.palette.as_mut().accent_yellow = sync_chroma_lightness(accent, yellow);

    let ext_blue = t.palette.as_mut().ext_blue;
    t.palette.as_mut().ext_blue = sync_chroma_lightness(accent, ext_blue);

    let ext_indigo = t.palette.as_mut().ext_indigo;
    t.palette.as_mut().ext_indigo = sync_chroma_lightness(accent, ext_indigo);

    let ext_orange = t.palette.as_mut().ext_orange;
    t.palette.as_mut().ext_orange = sync_chroma_lightness(accent, ext_orange);

    let ext_pink = t.palette.as_mut().ext_pink;
    t.palette.as_mut().ext_pink = sync_chroma_lightness(accent, ext_pink);

    let ext_purple = t.palette.as_mut().ext_purple;
    t.palette.as_mut().ext_purple = sync_chroma_lightness(accent, ext_purple);

    let ext_warm_grey = t.palette.as_mut().ext_warm_grey;
    t.palette.as_mut().ext_warm_grey = sync_chroma_lightness(accent, ext_warm_grey);

    let ext_yellow = t.palette.as_mut().ext_yellow;
    t.palette.as_mut().ext_yellow = sync_chroma_lightness(accent, ext_yellow);

    let bright_green = t.palette.as_mut().bright_green;
    t.palette.as_mut().bright_green =
        Lch::from_color(sync_chroma_lightness(accent, bright_green)).saturate(0.5).into_color();

    let bright_orange = t.palette.as_mut().bright_orange;
    t.palette.as_mut().bright_orange =
        Lch::from_color(sync_chroma_lightness(accent, bright_orange)).saturate(0.5).into_color();

    let bright_red = t.palette.as_mut().bright_red;
    t.palette.as_mut().bright_red =
        Lch::from_color(sync_chroma_lightness(accent, bright_red)).saturate(0.5).into_color();

    let accent_indigo = t.palette.as_mut().accent_indigo;
    t.palette.as_mut().accent_indigo = sync_chroma_lightness(accent, accent_indigo);

    let accent_pink = t.palette.as_mut().accent_pink;
    t.palette.as_mut().accent_pink = sync_chroma_lightness(accent, accent_pink);

    let accent_warm_grey = t.palette.as_mut().accent_warm_grey;
    t.palette.as_mut().accent_warm_grey = sync_chroma_lightness(accent, accent_warm_grey);

    let accent_yellow = t.palette.as_mut().accent_yellow;
    t.palette.as_mut().accent_yellow = sync_chroma_lightness(accent, accent_yellow);

    t.write_entry(&builder_config)?;

    let theme = t.build();

    let theme_config = if theme.is_dark { Theme::dark_config() } else { Theme::light_config() }?;
    theme.write_entry(&theme_config)?;

    Ok(())
}

fn sync_chroma_lightness(target: impl IntoColor<Lch>, c: impl IntoColor<Lch>) -> Srgba {
    let target = target.into_color();
    let mut c = c.into_color();
    c.chroma = target.chroma;
    c.l = target.l;
    c.clamp().into_color()
}

// binary search modifying a's lightness to satisfy contrast with b
fn adjust_lightness_for_contrast(original: Lch, b: Lch, cutoff: f32) -> Lch {
    let a_luma = SrgbLuma::from_color(original);
    let b_luma = SrgbLuma::from_color(b);

    if a_luma.has_min_contrast_text(b_luma) {
        return original;
    }

    let c_arr: Vec<(Lch, f32)> = (0..=40)
        .map(|i| {
            let mut c = original;
            c.l = 100. * i as f32 / 40.;
            c.clamp()
        })
        .map(|c| {
            let c_luma = SrgbLuma::from_color(c);
            let contrast = c_luma.relative_contrast(b_luma);
            (c, contrast)
        })
        .collect();
    let filtered = c_arr.iter().filter(|c| c.1 > cutoff).cloned().collect::<Vec<(Lch, f32)>>();
    filtered
        .into_iter()
        .min_by(|a, b| (a.0.l - original.l).abs().total_cmp(&(b.0.l - original.l).abs()))
        .map(|(c, _)| c)
        .unwrap_or_else(|| {
            c_arr
                .into_iter()
                .max_by(|a_1, a_2| a_1.1.total_cmp(&a_2.1))
                .map(|(c, _)| c)
                .unwrap_or(original)
        })
}

fn use_saved_result(path: &str, is_dark: bool) -> anyhow::Result<()> {
    let my_config = cosmic_config::Config::new_state(ID, 1)?;
    let result = my_config.get::<BgResult>(path)?;

    let builder_config =
        if is_dark { ThemeBuilder::dark_config()? } else { ThemeBuilder::light_config()? };

    let mut t = match ThemeBuilder::get_entry(&builder_config) {
        Ok(entry) => entry,
        Err((errs, entry)) => {
            for err in errs {
                tracing::error!("Failed to get the dark theme: {}", err);
            }
            entry
        },
    };

    t = t.accent(result.accent).bg_color(result.bg.into_color()).neutral_tint(result.neutral);

    if let Some(text) = result.text {
        t = t.text_tint(text);
    }

    t.write_entry(&builder_config)?;

    let theme = t.build();

    let theme_config = if theme.is_dark { Theme::dark_config() } else { Theme::light_config() }?;

    theme.write_entry(&theme_config)?;

    Ok(())
}

// TODO add palette colors
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BgResult {
    pub accent: Srgb,
    pub bg: Srgba,
    pub neutral: Srgb,
    pub text: Option<Srgb>,
}

/// Sorted colors
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KmeanState(pub Vec<Lab>);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, CosmicConfigEntry)]
#[version = 1]
pub struct MyConfig {
    pub avoid_accents_light: Vec<Srgb>,
    pub avoid_accents_dark: Vec<Srgb>,
    pub avoid_light: Vec<Srgb>,
    pub avoid_dark: Vec<Srgb>,
    pub save_results: bool,
    pub save_kmeans: bool,
    pub randomize: bool,
}

impl Default for MyConfig {
    fn default() -> Self {
        Self {
            avoid_accents_light: vec![
                Srgb::new(0.54, 0.38, 0.28),
                Srgb::new(0.47, 0.34, 0.14),
                Srgb::new(0.56078, 0.40784, 0.17647),
                Srgb::new(0.56078, 0.40784, 0.07),
                Srgb::new(0.651, 0.486, 0.443),
                Srgb::new(0.439, 0.431, 0.078),
                Srgb::new(0.47, 0.51, 0.32),
            ],
            avoid_accents_dark: vec![
                Srgb::new(0.54, 0.38, 0.28),
                Srgb::new(0.47, 0.34, 0.14),
                Srgb::new(0.56078, 0.40784, 0.17647),
                Srgb::new(0.56078, 0.40784, 0.07),
                Srgb::new(0.651, 0.486, 0.443),
                Srgb::new(0.439, 0.431, 0.078),
                Srgb::new(0.47, 0.51, 0.32),
            ],
            avoid_light: Vec::new(),
            avoid_dark: vec![
                Srgb::new(0.169, 0.165, 0.004),
                Srgb::new(0.169, 0.098, 0.004),
                Srgb::new(0.29, 0.18, 0.129),
                Srgb::new(0.29, 0.271, 0.129),
            ],
            save_results: false,
            save_kmeans: true,
            randomize: true,
        }
    }
}

fn left_skewed_shuffle<T>(mut v: Vec<T>, max_len_swap: Option<usize>) -> Vec<T> {
    let mut rng = rand::rng();
    let max_i = max_len_swap.unwrap_or(v.len());
    for i in 0..max_i {
        if i >= v.len() {
            return v;
        }
        let j = rng.random_range(i..v.len());
        v.swap(i, j);
    }
    v
}
