/// Game identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Game {
    Hoi4,
    Stellaris,
    Eu4,
    Ck2,
    Ck3,
    Vic2,
    Vic3,
    Ir,
    Eu5,
    Custom,
}

impl std::fmt::Display for Game {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Game::Hoi4 => write!(f, "hoi4"),
            Game::Stellaris => write!(f, "stellaris"),
            Game::Eu4 => write!(f, "eu4"),
            Game::Ck2 => write!(f, "ck2"),
            Game::Ck3 => write!(f, "ck3"),
            Game::Vic2 => write!(f, "vic2"),
            Game::Vic3 => write!(f, "vic3"),
            Game::Ir => write!(f, "ir"),
            Game::Eu5 => write!(f, "eu5"),
            Game::Custom => write!(f, "custom"),
        }
    }
}

impl Game {
    // Returns Option (no parse error type), so it can't be the Result-returning
    // std::str::FromStr; keeping the conventional `from_str` name.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "hoi4" => Some(Game::Hoi4),
            "stellaris" | "stl" => Some(Game::Stellaris),
            "eu4" => Some(Game::Eu4),
            "ck2" => Some(Game::Ck2),
            "ck3" => Some(Game::Ck3),
            "vic2" => Some(Game::Vic2),
            "vic3" => Some(Game::Vic3),
            "ir" | "imperator" => Some(Game::Ir),
            "eu5" => Some(Game::Eu5),
            "custom" => Some(Game::Custom),
            _ => None,
        }
    }
}
