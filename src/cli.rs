//! Command-line argument parsing for harbour.

use clap::Parser;

/// Curated torrents straight from your terminal.
#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(
    name = "harbour",
    version,
    about = "Curated torrents straight from your terminal",
    long_about = "Curated torrents straight from your terminal.\n\nA terminal client for searching curated torrents across 10 sources and managing background downloads."
)]
pub struct Cli {
    /// Optional target to download immediately (magnet URI, 40-char infohash, or .torrent file path).
    #[arg(value_name = "TARGET")]
    pub target: Option<String>,
}

impl Cli {
    /// Parse command-line arguments from environment.
    pub fn parse_args() -> Self {
        Self::parse()
    }

    /// Parse command-line arguments from an iterator (useful for testing).
    pub fn parse_from_iter<I, T>(itr: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        Self::try_parse_from(itr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn test_cli_no_args() {
        let cli = Cli::parse_from_iter(["harbour"]).expect("should parse empty args");
        assert_eq!(cli.target, None);
    }

    #[test]
    fn test_cli_target_arg() {
        let cli = Cli::parse_from_iter(["harbour", "ubuntu.torrent"]).expect("should parse target");
        assert_eq!(cli.target.as_deref(), Some("ubuntu.torrent"));
    }

    #[test]
    fn test_cli_version_flag() {
        let err = Cli::parse_from_iter(["harbour", "--version"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
    }

    #[test]
    fn test_cli_help_flag() {
        let err = Cli::parse_from_iter(["harbour", "--help"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
    }
}
