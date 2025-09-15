# COSMIC Background Wallpaper

Unofficial service for syncing the theme with the wallpaper for the COSMIC(tm) desktop.

## Table of Contents

- [COSMIC Background Wallpaper](#cosmic-background-wallpaper)
  - [Table of Contents](#table-of-contents)
  - [Installation](#installation)
  - [Usage](#usage)
  - [License](#license)

## Installation

`cargo build --release && sudo make install`

## Usage

Run `cosmic-ext-bg-theme`, or set it to auto start in cosmic-settings

Generated palettes for each wallpaper are saved in `$XDG_STATE_HOME/cosmic/gay.ash.CosmicBgTheme`. You can clear them or edit them to customize the generated values. The suffix of the file name marks them as dark or light palettes. true => dark and false => light

## License

GPL-3.0-only
