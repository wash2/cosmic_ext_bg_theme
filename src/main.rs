use cosmic_bg_config::state::State;
use cosmic_config::{ConfigGet, ConfigSet, CosmicConfigEntry};
use cosmic_settings_daemon::{ConfigProxy, CosmicSettingsDaemonProxy};
use cosmic_theme::{Theme, ThemeBuilder};
use futures::stream::Stream;
use futures::StreamExt;
use image::GenericImageView;
use kmeans_colors::{get_kmeans, Kmeans, Sort};
use palette::color_difference::Wcag21RelativeContrast;
use palette::{Clamp, FromColor, IntoColor, Lab, Lch, Srgb, SrgbLuma, Srgba};
use serde::{Deserialize, Serialize};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};
use zbus::Connection;

const ID: &str = "gay.ash.CosmicBgTheme";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let fmt_layer = fmt::layer().with_target(false);
    let filter_layer =
        EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info")).unwrap();
    if let Ok(journal_layer) = tracing_journald::layer() {
        tracing_subscriber::registry().with(journal_layer).with(filter_layer).init();
    } else {
        tracing_subscriber::registry().with(fmt_layer).with(filter_layer).init();
    }

    log_panics::init();
    tracing::info!("Starting CosmicBgTheme");
    let settings_proxy = connect_settings_daemon().await?;
    let config = State::state()?;
    let (path, name) = settings_proxy.watch_state(cosmic_bg_config::NAME, State::version()).await?;
    let bg_state_proxy = ConfigProxy::builder(settings_proxy.connection())
        .path(path)?
        .destination(name)?
        .build()
        .await?;

    let mut state = match State::get_entry(&config) {
        Ok(entry) => entry,
        Err((errs, entry)) => {
            for err in errs {
                tracing::error!("Failed to get the current state: {}", err);
            }
            entry
        },
    };

    if let Err(err) = apply_state(&state, true) {
        tracing::error!("Failed to apply the state: {}", err);
    }
    if let Err(err) = apply_state(&state, false) {
        tracing::error!("Failed to apply the state: {}", err);
    }
    let mut changes = bg_state_proxy.receive_changed().await?;

    let mut ownership_change = settings_proxy.receive_owner_changed().await?;

    loop {
        let c = tokio::select! {
            c = changes.next() => c,
            c = ownership_change.next() => {
                if c.is_none() {
                    // The settings daemon has exited
                    std::process::exit(0);
                } else {
                    None
                }
            },
        };
        let c = match c {
            Some(c) => c,
            None => break,
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

        if let Err(err) = apply_state(&state, true) {
            tracing::error!("Failed to apply the state: {}", err);
        }
        if let Err(err) = apply_state(&state, false) {
            tracing::error!("Failed to apply the state: {}", err);
        }
    }

    Ok(())
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

fn apply_state(state: &State, is_dark: bool) -> anyhow::Result<()> {
    let Some(w) = state.wallpapers.get(0) else {
        anyhow::bail!("No wallpapers found");
    };
    let cosmic_bg_config::Source::Path(ref path) = &w.1 else {
        anyhow::bail!("No wallpaper path");
    };

    let p = format!("{}_{}", path.to_string_lossy().replace("/", "_"), is_dark);
    if use_saved_result(&p, is_dark).is_ok() {
        return Ok(());
    }

    let img: Vec<Lab> = image::open(path)?
        .pixels()
        .map(|(_, _, p)| {
            let p = p.0;
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

    let mut kmeans = results.into_iter().last().unwrap();
    let mut res = Lab::sort_indexed_colors(&kmeans.centroids, &kmeans.indices);
    res.sort_unstable_by(|a, b| (b.percentage).total_cmp(&a.percentage));

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

    let new_window_bg = res.remove(0).centroid;
    kmeans.centroids.retain(|c| c != &new_window_bg.into_color());
    let mut new_window_bg: Lch = new_window_bg.into_color();
    if (new_window_bg.chroma - default_window_bg.chroma).abs() > 15. {
        new_window_bg.chroma = default_window_bg.chroma + 15.;
        new_window_bg = new_window_bg.clamp();
    }
    new_window_bg =
        adjust_lightness_for_contrast(new_window_bg, default.accent.base.into_color(), 6.);

    t = t.bg_color(new_window_bg.into_color());

    // ACCENT
    let clay_brown: Lch = Srgb::new(0.54, 0.38, 0.28).into_color();
    let muddy_brown: Lch = Srgb::new(0.47, 0.34, 0.14).into_color();
    let brown: Lch = Srgb::new(0.56078, 0.40784, 0.17647).into_color();
    let gross_yellow: Lch = Srgb::new(0.439, 0.431, 0.078).into_color();
    let gross_green: Lch = Srgb::new(0.47, 0.51, 0.32).into_color();
    let avoid = vec![clay_brown, muddy_brown, brown, gross_yellow, gross_green];

    let mut accent: (Lab, Lch) = (kmeans.centroids[0], kmeans.centroids[0].into_color());
    let mut best = f32::MIN;
    for color in &kmeans.centroids {
        let adjusted = adjust_lightness_for_contrast(
            (*color).into_color(),
            default.background.base.into_color(),
            4.5,
        );
        let mut score = adjusted.chroma;
        if avoid.iter().any(|c| {
            let hue_diff = (adjusted.hue.into_inner() - c.hue.into_inner()).abs() % 180.;
            (adjusted.chroma - c.chroma).powf(2.) + (hue_diff).powf(2.) < 666. && hue_diff < 20.
        }) {
            score /= 10.;
        }
        if score > best {
            best = score;
            accent = (*color, adjusted);
        }
    }
    let max_hue_diff = kmeans
        .centroids
        .iter()
        .map(|c| {
            let c = Lch::from_color(*c);
            (c.hue - accent.1.hue).into_inner().abs()
        })
        .max_by(|a, b| a.total_cmp(b))
        .unwrap();
    kmeans.centroids.retain(|c| {
        let c = Lch::from_color(*c);
        (c.hue - accent.1.hue).into_inner().abs() > max_hue_diff / 6.
    });
    res.retain(|c| {
        let c = Lch::from_color(c.centroid);
        (c.hue - accent.1.hue).into_inner().abs() > max_hue_diff / 6.
    });

    let accent = Srgb::from_color(accent.1);
    t = t.accent(accent);

    // NEUTRAL
    let mut neutral = default.palette.neutral_5;

    for c in res {
        let c_lch = Lch::from_color(c.centroid);
        if c_lch.chroma > 10. {
            neutral = c_lch.into_color();
            break;
        }
    }

    t = t.neutral_tint(neutral.into_color());

    t.write_entry(&builder_config)?;

    let result = BgResult { accent, bg: t.bg_color.unwrap(), neutral: t.neutral_tint.unwrap() };
    let my_config = cosmic_config::Config::new_state(ID, 1)?;
    if let Err(err) = my_config.set(&p, result) {
        tracing::error!("Failed to save the result: {}", err);
    }

    let theme = t.build();

    let theme_config = if theme.is_dark { Theme::dark_config() } else { Theme::light_config() }?;

    theme.write_entry(&theme_config)?;

    Ok(())
}

// binary search modifying a's lightness to satisfy contrast with b
fn adjust_lightness_for_contrast(original: Lch, b: Lch, cutoff: f32) -> Lch {
    let a_luma = SrgbLuma::from_color(original);
    let b_luma = SrgbLuma::from_color(b);

    if a_luma.has_min_contrast_text(b_luma) {
        return original;
    }

    let c_arr: Vec<(Lch, f32)> = (0..=40)
        .into_iter()
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
        .clone()
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

    t.write_entry(&builder_config)?;

    let theme = t.build();

    let theme_config = if theme.is_dark { Theme::dark_config() } else { Theme::light_config() }?;

    theme.write_entry(&theme_config)?;

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BgResult {
    pub accent: Srgb,
    pub bg: Srgba,
    pub neutral: Srgb,
}
