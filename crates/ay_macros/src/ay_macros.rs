use proc_macro::TokenStream;

/// Compile-time Tailwind v4 utility resolver into typed `ay_ui::StyleSpec`.
/// Fails compilation on unknown utility tokens (fail-fast, zero runtime CSS parser).
#[proc_macro]
pub fn tw(input: TokenStream) -> TokenStream {
    let raw = input.to_string();
    let trimmed = raw.trim();
    let Some(inner) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return "compile_error!(\"tw! expects a single string literal of Tailwind utility classes\")"
            .parse()
            .unwrap_or_default();
    };

    let mut flex = false;
    let mut col = false;
    let mut gap_dp: u16 = 0;
    let mut pad_dp: u16 = 0;
    let mut radius = "none";
    let mut bg = "Surface";
    let mut fg = "OnSurface";

    for token in inner.split_whitespace() {
        match token {
            "flex" => flex = true,
            "flex-col" => {
                flex = true;
                col = true;
            }
            "flex-row" => {
                flex = true;
                col = false;
            }
            "rounded-none" => radius = "none",
            "rounded-sm" => radius = "sm",
            "rounded-md" | "rounded" => radius = "md",
            "rounded-lg" => radius = "lg",
            "rounded-xl" => radius = "xl",
            "rounded-full" => radius = "full",
            "bg-surface" => bg = "Surface",
            "bg-surface-low" => bg = "SurfaceContainerLow",
            "bg-surface-high" => bg = "SurfaceContainerHigh",
            "bg-primary" => bg = "Primary",
            "bg-secondary" => bg = "SecondaryContainer",
            "bg-error" => bg = "Error",
            "text-fg" => fg = "OnSurface",
            "text-muted" => fg = "OnSurfaceVariant",
            "text-on-primary" => fg = "OnPrimary",
            "text-primary" => fg = "Primary",
            other if other.starts_with("gap-") => {
                if let Ok(n) = other[4..].parse::<u16>() {
                    gap_dp = n.saturating_mul(4);
                } else {
                    return format!("compile_error!(\"tw!: invalid gap token '{other}'\")")
                        .parse()
                        .unwrap_or_default();
                }
            }
            other if other.starts_with("p-") => {
                if let Ok(n) = other[2..].parse::<u16>() {
                    pad_dp = n.saturating_mul(4);
                } else {
                    return format!("compile_error!(\"tw!: invalid padding token '{other}'\")")
                        .parse()
                        .unwrap_or_default();
                }
            }
            unknown => {
                return format!("compile_error!(\"tw!: unknown Tailwind utility '{unknown}'\")")
                    .parse()
                    .unwrap_or_default();
            }
        }
    }

    format!(
        "::ay_ui::StyleSpec {{ flex: {flex}, column: {col}, gap_dp: {gap_dp}, padding_dp: {pad_dp}, radius_token: \"{radius}\", bg_role: \"{bg}\", fg_role: \"{fg}\" }}"
    )
    .parse()
    .unwrap_or_default()
}
