#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThemeTokens {
    pub name: &'static str,
    pub colors: ThemeColors,
    pub radii: ThemeRadii,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ThemeKind {
    #[default]
    Ember,
    Graphite,
    Daylight,
}

impl ThemeKind {
    pub const ALL: [Self; 3] = [Self::Ember, Self::Graphite, Self::Daylight];

    pub fn storage_name(self) -> &'static str {
        match self {
            Self::Ember => "ember",
            Self::Graphite => "graphite",
            Self::Daylight => "daylight",
        }
    }

    pub fn from_storage_name(value: &str) -> Self {
        match value {
            "ember" => Self::Ember,
            "daylight" => Self::Daylight,
            _ => Self::Graphite,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Ember => Self::Graphite,
            Self::Graphite => Self::Daylight,
            Self::Daylight => Self::Ember,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThemeColors {
    pub root: u32,
    pub chrome: u32,
    pub panel: u32,
    pub raised: u32,
    pub overlay: u32,
    pub text: u32,
    pub muted_text: u32,
    pub faint_text: u32,
    pub border: u32,
    pub strong_border: u32,
    pub focus_ring: u32,
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_soft: u32,
    pub on_accent: u32,
    pub success: u32,
    pub warning: u32,
    pub warning_soft: u32,
    pub error: u32,
    pub info: u32,
    pub addition: u32,
    pub addition_soft: u32,
    pub deletion: u32,
    pub deletion_soft: u32,
    pub shadow: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThemeRadii {
    pub small: u16,
    pub medium: u16,
    pub large: u16,
}

pub(crate) const GRAPHITE: ThemeTokens = ThemeTokens {
    name: "Graphite",
    colors: ThemeColors {
        root: 0x0f1115,
        chrome: 0x17191f,
        panel: 0x191c22,
        raised: 0x20232a,
        overlay: 0x242831,
        text: 0xd1d5db,
        muted_text: 0x9ca3af,
        faint_text: 0x767d8d,
        border: 0x292c33,
        strong_border: 0x3a3f4b,
        focus_ring: 0x60a5fa,
        accent: 0x2563eb,
        accent_hover: 0x3b82f6,
        accent_soft: 0x202a3d,
        on_accent: 0xf8fafc,
        success: 0x3fb950,
        warning: 0xfbbf24,
        warning_soft: 0x3b2d13,
        error: 0xf87171,
        info: 0x93c5fd,
        addition: 0x3fb950,
        addition_soft: 0x123523,
        deletion: 0xf85149,
        deletion_soft: 0x431d22,
        shadow: 0x00000099,
    },
    radii: ThemeRadii {
        small: 4,
        medium: 6,
        large: 10,
    },
};

pub(crate) const EMBER: ThemeTokens = ThemeTokens {
    name: "Ember",
    colors: ThemeColors {
        root: 0x130f0d,
        chrome: 0x1d1713,
        panel: 0x241c17,
        raised: 0x2d231d,
        overlay: 0x382a22,
        text: 0xf5e9df,
        muted_text: 0xc3aa98,
        faint_text: 0x8f7767,
        border: 0x46362c,
        strong_border: 0x654b3b,
        focus_ring: 0xfb923c,
        accent: 0xea580c,
        accent_hover: 0xf97316,
        accent_soft: 0x422515,
        on_accent: 0xfff7ed,
        success: 0x4ade80,
        warning: 0xfacc15,
        warning_soft: 0x3b2b12,
        error: 0xfb7185,
        info: 0x7dd3fc,
        addition: 0x4ade80,
        addition_soft: 0x173a27,
        deletion: 0xfb7185,
        deletion_soft: 0x4a1f28,
        shadow: 0x090503a8,
    },
    radii: ThemeRadii {
        small: 4,
        medium: 7,
        large: 11,
    },
};

pub(crate) const DAYLIGHT: ThemeTokens = ThemeTokens {
    name: "Daylight",
    colors: ThemeColors {
        root: 0xf4f6f8,
        chrome: 0xe9edf2,
        panel: 0xffffff,
        raised: 0xffffff,
        overlay: 0xdde4ec,
        text: 0x18212f,
        muted_text: 0x586577,
        faint_text: 0x7a8798,
        border: 0xcbd3dd,
        strong_border: 0xaeb9c7,
        focus_ring: 0x2563eb,
        accent: 0x2563eb,
        accent_hover: 0x1d4ed8,
        accent_soft: 0xdbeafe,
        on_accent: 0xffffff,
        success: 0x15803d,
        warning: 0xa16207,
        warning_soft: 0xfef3c7,
        error: 0xb91c1c,
        info: 0x0369a1,
        addition: 0x15803d,
        addition_soft: 0xdcfce7,
        deletion: 0xb91c1c,
        deletion_soft: 0xfee2e2,
        shadow: 0x17203333,
    },
    radii: ThemeRadii {
        small: 4,
        medium: 6,
        large: 10,
    },
};

pub(crate) fn tokens(kind: ThemeKind) -> &'static ThemeTokens {
    match kind {
        ThemeKind::Ember => &EMBER,
        ThemeKind::Graphite => &GRAPHITE,
        ThemeKind::Daylight => &DAYLIGHT,
    }
}

#[cfg(test)]
mod tests {
    use super::{DAYLIGHT, EMBER, GRAPHITE, ThemeKind};

    #[test]
    fn every_theme_defines_the_foundation_tokens() {
        for theme in [EMBER, GRAPHITE, DAYLIGHT] {
            assert!(!theme.name.is_empty());
            let colors = theme.colors;
            for color in [
                colors.root,
                colors.chrome,
                colors.panel,
                colors.raised,
                colors.overlay,
                colors.text,
                colors.muted_text,
                colors.faint_text,
                colors.border,
                colors.strong_border,
                colors.focus_ring,
                colors.accent,
                colors.accent_hover,
                colors.accent_soft,
                colors.on_accent,
                colors.success,
                colors.warning,
                colors.warning_soft,
                colors.error,
                colors.info,
                colors.addition,
                colors.addition_soft,
                colors.deletion,
                colors.deletion_soft,
                colors.shadow,
            ] {
                assert_ne!(color, 0, "theme colors must be explicitly defined");
            }
            assert_ne!(colors.root, colors.text);
            assert_ne!(colors.accent, colors.on_accent);
            assert_ne!(colors.addition_soft, colors.addition);
            assert_ne!(colors.deletion_soft, colors.deletion);
            assert!(theme.radii.small <= theme.radii.medium);
            assert!(theme.radii.medium <= theme.radii.large);
        }
    }

    #[test]
    fn theme_names_round_trip_and_cycle() {
        for theme in ThemeKind::ALL {
            assert_eq!(ThemeKind::from_storage_name(theme.storage_name()), theme);
        }
        assert_eq!(ThemeKind::Ember.next(), ThemeKind::Graphite);
        assert_eq!(ThemeKind::Graphite.next(), ThemeKind::Daylight);
        assert_eq!(ThemeKind::Daylight.next(), ThemeKind::Ember);
    }
}
