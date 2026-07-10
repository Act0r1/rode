#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThemeTokens {
    pub name: &'static str,
    pub colors: ThemeColors,
    pub radii: ThemeRadii,
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
    pub success: u32,
    pub warning: u32,
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
        success: 0x3fb950,
        warning: 0xfbbf24,
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

pub(crate) fn current() -> &'static ThemeTokens {
    &GRAPHITE
}

#[cfg(test)]
mod tests {
    use super::GRAPHITE;

    #[test]
    fn graphite_defines_every_foundation_token() {
        let theme = GRAPHITE;
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
            colors.success,
            colors.warning,
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
        assert!(theme.radii.small <= theme.radii.medium);
        assert!(theme.radii.medium <= theme.radii.large);
    }
}
