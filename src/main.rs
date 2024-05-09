use cosmic_bg_config::state::State;
use cosmic_config::{ConfigGet, ConfigSet, CosmicConfigEntry};
use cosmic_settings_daemon::{ConfigProxyBlocking, CosmicSettingsDaemonProxyBlocking};
use cosmic_theme::{Theme, ThemeBuilder, ThemeMode};
use image::GenericImageView;
use kmeans_colors::{get_kmeans, Kmeans, Sort};
use palette::color_difference::Wcag21RelativeContrast;
use palette::{Clamp, FromColor, IntoColor, Lab, Lch, Srgb, SrgbLuma, Srgba};
use serde::{Deserialize, Serialize};
use zbus::blocking::Connection;

const ID: &str = "gay.ash.CosmicBgTheme";

fn main() -> anyhow::Result<()> {
    let settings_proxy = connect_settings_daemon()?;
    let config = State::state()?;
    let (path, name) = settings_proxy.watch_state(cosmic_bg_config::NAME, State::version())?;
    let bg_state_proxy = ConfigProxyBlocking::builder(settings_proxy.connection())
        .path(path)?
        .destination(name)?
        .build()?;

    let mut state = match State::get_entry(&config) {
        Ok(entry) => entry,
        Err((errs, entry)) => {
            for err in errs {
                eprintln!("Failed to get the current state: {}", err);
            }
            entry
        },
    };

    if let Err(err) = apply_state(&state) {
        eprintln!("Failed to apply the state: {}", err);
    }
    let changes = bg_state_proxy.receive_changed()?;

    for c in changes {
        let Ok(args) = c.args() else {
            continue;
        };
        let (errors, keys) = state.update_keys(&config, &[args.key]);
        if keys.is_empty() {
            continue;
        }
        for err in errors {
            eprintln!("Failed to update the state: {}", err);
        }

        if let Err(err) = apply_state(&state) {
            eprintln!("Failed to apply the state: {}", err);
        }
    }

    Ok(())
}

fn load_conn() -> anyhow::Result<Connection> {
    for _ in 0..5 {
        match Connection::session() {
            Ok(conn) => return Ok(conn),
            Err(e) => {
                eprintln!("Failed to connect to the session bus: {}", e);
                std::thread::sleep(std::time::Duration::from_secs(1));
            },
        }
    }
    Err(anyhow::anyhow!("Failed to connect to the session bus"))
}

fn connect_settings_daemon() -> anyhow::Result<CosmicSettingsDaemonProxyBlocking<'static>> {
    let conn = load_conn()?;
    for _ in 0..5 {
        match CosmicSettingsDaemonProxyBlocking::builder(&conn).build() {
            Ok(proxy) => return Ok(proxy),
            Err(e) => {
                eprintln!("Failed to connect to the settings daemon: {}", e);
                std::thread::sleep(std::time::Duration::from_secs(1));
            },
        }
    }
    Err(anyhow::anyhow!("Failed to connect to the settings daemon"))
}

fn apply_state(state: &State) -> anyhow::Result<()> {
    let Some(w) = state.wallpapers.get(0) else {
        anyhow::bail!("No wallpapers found");
    };
    let cosmic_bg_config::Source::Path(ref path) = &w.1 else {
        anyhow::bail!("No wallpaper path");
    };

    let t = match cosmic_theme::ThemeMode::get_entry(&ThemeMode::config()?) {
        Ok(entry) => entry,
        Err((errs, entry)) => {
            for err in errs {
                eprintln!("Failed to get the current theme mode: {}", err);
            }
            entry
        },
    };

    let p = format!("{}_{}", path.to_string_lossy().replace("/", "_"), t.is_dark);
    if use_saved_result(&p).is_ok() {
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

    let (builder_config, default) = if t.is_dark {
        (ThemeBuilder::dark_config()?, Theme::dark_default())
    } else {
        (ThemeBuilder::light_config()?, Theme::light_default())
    };

    let mut t = match ThemeBuilder::get_entry(&builder_config) {
        Ok(entry) => entry,
        Err((errs, entry)) => {
            for err in errs {
                eprintln!("Failed to get the dark theme: {}", err);
            }
            entry
        },
    };

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

    let default_window_bg = Lch::from_color(default.background.base);

    let mut nearest_to_window_bg = find_nearest_lch(default_window_bg, &kmeans.centroids);
    nearest_to_window_bg =
        adjust_lightness_for_contrast(nearest_to_window_bg, default.background.on.into_color());

    t = t.bg_color(nearest_to_window_bg.into_color());

    // use most common color for the neutral color
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

    let theme = t.build();

    let theme_config = if theme.is_dark { Theme::dark_config() } else { Theme::light_config() }?;

    theme.write_entry(&theme_config)?;

    let my_config = cosmic_config::Config::new_state(ID, 1)?;

    if let Err(err) = my_config.set(&p, result) {
        eprintln!("Failed to save the result: {}", err);
    }

    Ok(())
}

fn find_nearest_lch(c: Lch, colors: &[Lab]) -> Lch {
    let mut best = f32::MAX;
    let mut nearest = c;
    for color in colors {
        let mut lch = Lch::from_color(*color);
        if (lch.l - c.l).abs() > 20. {
            lch.l = c.l;
        }

        let score = (c.l - lch.l).abs() + (c.chroma - lch.chroma).abs();
        if score < best {
            best = score;
            nearest = lch;
        }
    }
    nearest
}

// binary search modifying a's lightness to satisfy contrast with b
fn adjust_lightness_for_contrast(original: Lch, b: Lch) -> Lch {
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
    let filtered = c_arr.iter().filter(|c| c.1 > 4.5).cloned().collect::<Vec<(Lch, f32)>>();
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

fn use_saved_result(path: &str) -> anyhow::Result<()> {
    let my_config = cosmic_config::Config::new_state(ID, 1)?;
    let result = my_config.get::<BgResult>(path)?;
    let t = match cosmic_theme::ThemeMode::get_entry(&ThemeMode::config()?) {
        Ok(entry) => entry,
        Err((errs, entry)) => {
            for err in errs {
                eprintln!("Failed to get the current theme mode: {}", err);
            }
            entry
        },
    };

    let builder_config =
        if t.is_dark { ThemeBuilder::dark_config()? } else { ThemeBuilder::light_config()? };

    let mut t = match ThemeBuilder::get_entry(&builder_config) {
        Ok(entry) => entry,
        Err((errs, entry)) => {
            for err in errs {
                eprintln!("Failed to get the dark theme: {}", err);
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
