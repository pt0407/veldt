// Visual ecosystem viewer — generates a garden-themed HTML visualization

use crate::veldt::{Health, Veldt};
use std::io::Write;

pub fn generate_visual(veldt: &Veldt) -> Result<String, String> {
    let data = serde_json::json!({
        "functions": veldt.functions.iter().map(|(name, variants)| {
            serde_json::json!({
                "name": name,
                "variants": variants.iter().map(|e| {
                    let health_str = match &e.health {
                        Health::Healthy => "healthy",
                        Health::Sick(_) => "sick",
                        Health::Dying => "dying",
                    };
                    let health_detail = match &e.health {
                        Health::Healthy => "".to_string(),
                        Health::Sick(e) => e.clone(),
                        Health::Dying => "".into(),
                    };
                    serde_json::json!({
                        "id": e.id,
                        "health": health_str,
                        "health_detail": health_detail,
                        "age": e.age,
                        "lifespan": (e.lifespan as i64),
                        "usage": e.usage_count,
                        "fitness": (e.fitness * 100.0) as i64,
                        "trial_score": (e.trial_score * 100.0) as i64,
                        "last_used": e.last_used,
                        "healing_attempts": e.healing_attempts.len(),
                        "born": e.born,
                    })
                }).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "variables": veldt.variables.keys().collect::<Vec<_>>(),
        "structs": veldt.structs.iter().map(|(name, entry)| {
            serde_json::json!({
                "name": name,
                "fields": entry.fields.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "obituaries": veldt.obituaries.iter().rev().take(30).map(|o| {
            let health_str = match &o.health {
                Health::Healthy => "healthy",
                Health::Sick(_) => "sick",
                Health::Dying => "dying",
            };
            serde_json::json!({
                "name": o.name,
                "id": o.id,
                "age": o.age,
                "health": health_str,
                "cause": o.cause_of_death,
                "last_used": o.last_used,
            })
        }).collect::<Vec<_>>(),
        "run_count": veldt.run_count,
    });

    let data_json = serde_json::to_string(&data)
        .map_err(|e| format!("Failed to serialize: {}", e))?;

    let html = format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Veldt — Garden</title>
<style>
  @import url('https://fonts.googleapis.com/css2?family=Cormorant+Garamond:ital,wght@0,300;0,400;0,500;1,300&family=JetBrains+Mono:wght@300;400;500&display=swap');

  * {{ margin: 0; padding: 0; box-sizing: border-box; }}

  :root {{
    --bg-deep: #050807;
    --bg-soil: #0c1210;
    --bg-card: #0f1614;
    --moss: #4a7c59;
    --moss-bright: #6fae7f;
    --moss-glow: rgba(111, 174, 127, 0.15);
    --bloom: #8ec5a0;
    --amber: #c9a96e;
    --rust: #b85450;
    --ash: #5a6560;
    --fog: #8a9590;
    --text: #b8c8be;
    --text-dim: #5a6b62;
    --border: rgba(74, 124, 89, 0.15);
    --border-bright: rgba(111, 174, 127, 0.3);
  }}

  body {{
    background: var(--bg-deep);
    color: var(--text);
    font-family: 'JetBrains Mono', monospace;
    font-weight: 300;
    min-height: 100vh;
    overflow-x: hidden;
  }}

  /* Atmospheric background */
  .atmosphere {{
    position: fixed;
    inset: 0;
    z-index: 0;
    pointer-events: none;
    background:
      radial-gradient(ellipse 80% 50% at 50% 0%, rgba(74, 124, 89, 0.08), transparent 60%),
      radial-gradient(ellipse 60% 40% at 30% 100%, rgba(201, 169, 110, 0.04), transparent 70%),
      radial-gradient(ellipse 50% 30% at 80% 60%, rgba(111, 174, 127, 0.03), transparent 70%);
  }}

  .grain {{
    position: fixed;
    inset: 0;
    z-index: 1;
    pointer-events: none;
    opacity: 0.03;
    background-image: url("data:image/svg+xml,%3Csvg viewBox='0 0 200 200' xmlns='http://www.w3.org/2000/svg'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.9' /%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)' /%3E%3C/svg%3E");
  }}

  .container {{
    position: relative;
    z-index: 2;
    max-width: 1100px;
    margin: 0 auto;
    padding: 60px 32px 120px;
  }}

  /* Header */
  header {{
    text-align: center;
    margin-bottom: 80px;
    animation: fadeIn 1.5s ease;
  }}

  @keyframes fadeIn {{
    from {{ opacity: 0; transform: translateY(20px); }}
    to {{ opacity: 1; transform: translateY(0); }}
  }}

  @keyframes growIn {{
    from {{ opacity: 0; transform: translateY(40px) scale(0.95); }}
    to {{ opacity: 1; transform: translateY(0) scale(1); }}
  }}

  @keyframes sway {{
    0%, 100% {{ transform: rotate(-0.5deg); }}
    50% {{ transform: rotate(0.5deg); }}
  }}

  @keyframes pulse {{
    0%, 100% {{ opacity: 0.4; }}
    50% {{ opacity: 0.7; }}
  }}

  @keyframes float {{
    0%, 100% {{ transform: translateY(0px) rotate(0deg); }}
    50% {{ transform: translateY(-8px) rotate(2deg); }}
  }}

  h1 {{
    font-family: 'Cormorant Garamond', serif;
    font-size: 64px;
    font-weight: 300;
    letter-spacing: -1px;
    color: var(--bloom);
    margin-bottom: 8px;
    text-shadow: 0 0 40px rgba(111, 174, 127, 0.2);
  }}

  h1 em {{
    font-style: italic;
    font-weight: 300;
    color: var(--amber);
  }}

  .tagline {{
    font-size: 13px;
    color: var(--text-dim);
    letter-spacing: 2px;
    text-transform: uppercase;
  }}

  /* Stats */
  .stats {{
    display: flex;
    justify-content: center;
    gap: 48px;
    margin-bottom: 80px;
    animation: fadeIn 2s ease 0.3s both;
  }}

  .stat {{
    text-align: center;
  }}

  .stat .num {{
    font-family: 'Cormorant Garamond', serif;
    font-size: 42px;
    font-weight: 400;
    color: var(--moss-bright);
    line-height: 1;
  }}

  .stat.sick .num {{ color: var(--rust); }}
  .stat.dead .num {{ color: var(--ash); }}
  .stat.runs .num {{ color: var(--amber); }}

  .stat .label {{
    font-size: 10px;
    color: var(--text-dim);
    letter-spacing: 1.5px;
    text-transform: uppercase;
    margin-top: 6px;
  }}

  /* Section headers */
  .section-label {{
    font-family: 'Cormorant Garamond', serif;
    font-size: 28px;
    font-weight: 400;
    font-style: italic;
    color: var(--fog);
    margin-bottom: 32px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }}

  .section-label .count {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 11px;
    font-style: normal;
    color: var(--text-dim);
    letter-spacing: 1px;
  }}

  /* Garden bed — where plants grow */
  .garden-bed {{
    margin-bottom: 80px;
  }}

  .ground {{
    position: relative;
    padding-top: 20px;
  }}

  .ground::before {{
    content: '';
    position: absolute;
    left: -32px;
    right: -32px;
    top: 0;
    height: 1px;
    background: linear-gradient(90deg, transparent, var(--border-bright), transparent);
  }}

  /* Plant — each lineage is a plant */
  .plant {{
    margin-bottom: 48px;
    animation: growIn 0.8s ease both;
  }}

  .plant-header {{
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 20px;
    padding-left: 8px;
  }}

  .plant-icon {{
    width: 28px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
  }}

  .plant-icon svg {{
    width: 100%;
    height: 100%;
  }}

  .plant-name {{
    font-family: 'Cormorant Garamond', serif;
    font-size: 22px;
    font-weight: 500;
    color: var(--bloom);
    letter-spacing: 0.5px;
  }}

  .plant-variants-count {{
    font-size: 11px;
    color: var(--text-dim);
    letter-spacing: 1px;
  }}

  /* Branches — variants of a lineage */
  .branches {{
    display: flex;
    gap: 16px;
    flex-wrap: wrap;
    padding-left: 36px;
    position: relative;
  }}

  .branches::before {{
    content: '';
    position: absolute;
    left: 14px;
    top: -8px;
    bottom: 0;
    width: 1px;
    background: linear-gradient(180deg, var(--border-bright), transparent);
  }}

  /* Leaf — each variant is a leaf */
  .leaf {{
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 4px 4px 4px 24px;
    padding: 20px 24px;
    min-width: 220px;
    position: relative;
    transition: all 0.4s cubic-bezier(0.4, 0, 0.2, 1);
    cursor: default;
    animation: growIn 0.6s ease both;
  }}

  .leaf:hover {{
    border-color: var(--border-bright);
    transform: translateY(-3px);
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4), 0 0 24px var(--moss-glow);
  }}

  .leaf.healthy {{
    border-color: rgba(111, 174, 127, 0.25);
  }}

  .leaf.healthy::before {{
    content: '';
    position: absolute;
    left: 0;
    top: 50%;
    width: 3px;
    height: 60%;
    transform: translateY(-50%);
    background: var(--moss-bright);
    border-radius: 0 2px 2px 0;
    box-shadow: 0 0 12px var(--moss-glow);
  }}

  .leaf.sick {{
    border-color: rgba(184, 84, 80, 0.2);
  }}

  .leaf.sick::before {{
    content: '';
    position: absolute;
    left: 0;
    top: 50%;
    width: 3px;
    height: 60%;
    transform: translateY(-50%);
    background: var(--rust);
    border-radius: 0 2px 2px 0;
  }}

  .leaf.dying {{
    border-color: rgba(90, 101, 96, 0.15);
    opacity: 0.5;
  }}

  .leaf.dying::before {{
    content: '';
    position: absolute;
    left: 0;
    top: 50%;
    width: 3px;
    height: 60%;
    transform: translateY(-50%);
    background: var(--ash);
    border-radius: 0 2px 2px 0;
  }}

  .leaf.fittest {{
    box-shadow: 0 0 32px rgba(111, 174, 127, 0.12);
  }}

  .leaf.fittest .leaf-id::after {{
    content: ' ◆';
    color: var(--amber);
    font-size: 10px;
  }}

  .leaf-id {{
    font-size: 14px;
    font-weight: 500;
    color: var(--text);
    margin-bottom: 12px;
  }}

  .leaf-id .hash {{
    color: var(--text-dim);
  }}

  .leaf-health {{
    font-size: 10px;
    letter-spacing: 1px;
    text-transform: uppercase;
    margin-bottom: 16px;
    padding: 3px 10px;
    border-radius: 20px;
    display: inline-block;
  }}

  .leaf-health.healthy {{ color: var(--moss-bright); background: rgba(111, 174, 127, 0.08); }}
  .leaf-health.sick {{ color: var(--rust); background: rgba(184, 84, 80, 0.08); }}
  .leaf-health.dying {{ color: var(--ash); background: rgba(90, 101, 96, 0.08); }}

  .leaf-detail {{
    font-size: 10px;
    color: var(--rust);
    margin-bottom: 12px;
    font-style: italic;
    line-height: 1.4;
  }}

  .leaf-stats {{
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px 16px;
    margin-bottom: 12px;
  }}

  .leaf-stat {{
    display: flex;
    justify-content: space-between;
    font-size: 11px;
  }}

  .leaf-stat .key {{ color: var(--text-dim); }}
  .leaf-stat .val {{ color: var(--text); font-weight: 400; }}

  /* Lifespan vine */
  .vine {{
    height: 3px;
    background: rgba(74, 124, 89, 0.08);
    border-radius: 2px;
    overflow: hidden;
    margin-top: 8px;
  }}

  .vine-fill {{
    height: 100%;
    border-radius: 2px;
    transition: width 1s ease;
  }}

  .vine-fill.healthy {{
    background: linear-gradient(90deg, var(--moss), var(--moss-bright));
    box-shadow: 0 0 8px var(--moss-glow);
  }}

  .vine-fill.sick {{
    background: linear-gradient(90deg, var(--rust), #d46862);
  }}

  .vine-fill.dying {{
    background: var(--ash);
  }}

  /* Stones — immortal data */
  .stones {{
    display: flex;
    gap: 16px;
    flex-wrap: wrap;
    margin-bottom: 80px;
  }}

  .stone {{
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 20px 16px 24px 12px;
    padding: 14px 22px;
    font-size: 13px;
    color: var(--fog);
    display: flex;
    align-items: center;
    gap: 8px;
    transition: all 0.3s ease;
    animation: growIn 0.6s ease both;
  }}

  .stone:hover {{
    border-color: var(--border-bright);
    color: var(--text);
  }}

  .stone .glyph {{
    font-family: 'Cormorant Garamond', serif;
    font-size: 18px;
    color: var(--amber);
  }}

  .stone .fields {{
    font-size: 10px;
    color: var(--text-dim);
    margin-left: 4px;
  }}

  /* Fallen leaves — obituaries */
  .fallen {{
    margin-bottom: 40px;
  }}

  .fallen-leaf {{
    background: transparent;
    border-left: 2px solid var(--ash);
    padding: 14px 20px;
    margin-bottom: 12px;
    font-size: 12px;
    color: var(--text-dim);
    transition: all 0.3s ease;
    animation: growIn 0.5s ease both;
  }}

  .fallen-leaf:hover {{
    border-left-color: var(--amber);
    color: var(--text);
    padding-left: 24px;
  }}

  .fallen-leaf.healthy {{ border-left-color: rgba(111, 174, 127, 0.3); }}
  .fallen-leaf.sick {{ border-left-color: rgba(184, 84, 80, 0.3); }}

  .fallen-name {{
    font-family: 'Cormorant Garamond', serif;
    font-size: 16px;
    font-weight: 500;
    color: var(--fog);
    margin-bottom: 4px;
  }}

  .fallen-name .hash {{ color: var(--text-dim); }}

  .fallen-cause {{
    color: var(--amber);
    font-style: italic;
    margin-top: 4px;
  }}

  .fallen-meta {{
    font-size: 10px;
    letter-spacing: 0.5px;
    margin-top: 4px;
  }}

  /* Empty states */
  .empty {{
    text-align: center;
    padding: 60px 20px;
    color: var(--text-dim);
    font-family: 'Cormorant Garamond', serif;
    font-size: 18px;
    font-style: italic;
  }}

  /* Footer */
  footer {{
    text-align: center;
    margin-top: 80px;
    padding-top: 32px;
    border-top: 1px solid var(--border);
    font-size: 11px;
    color: var(--text-dim);
    letter-spacing: 1px;
  }}

  /* Scrollbar */
  ::-webkit-scrollbar {{ width: 6px; }}
  ::-webkit-scrollbar-track {{ background: var(--bg-deep); }}
  ::-webkit-scrollbar-thumb {{ background: var(--border-bright); border-radius: 3px; }}
</style>
</head>
<body>
<div class="atmosphere"></div>
<div class="grain"></div>

<div class="container">
  <header>
    <h1>Veldt <em>Garden</em></h1>
    <p class="tagline">A living ecosystem of competing code</p>
  </header>

  <div class="stats" id="stats"></div>

  <section class="garden-bed" id="garden-section">
    <div class="section-label">
      Living Code <span class="count" id="func-count"></span>
    </div>
    <div class="ground" id="garden"></div>
  </section>

  <section id="immortals-section">
    <div class="section-label">
      Stones &mdash; Immortal Data <span class="count" id="immortal-count"></span>
    </div>
    <div class="stones" id="immortals"></div>
  </section>

  <section id="obituaries-section">
    <div class="section-label">
      Fallen Leaves &mdash; Obituaries <span class="count" id="obit-count"></span>
    </div>
    <div class="fallen" id="obituaries"></div>
  </section>

  <footer>
    The veldt remembers every seed, every bloom, every withering.
  </footer>
</div>

<script>
const DATA = {data_json};

const PLANT_SVG = `<svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
  <path d="M12 22V12M12 12C12 12 8 8 4 8C4 8 6 12 12 12M12 12C12 12 16 8 20 8C20 8 18 12 12 12M12 12C12 12 10 6 12 2C12 2 14 6 12 12" stroke="currentColor" stroke-width="1" stroke-linecap="round" stroke-linejoin="round" opacity="0.6"/>
</svg>`;

function renderStats() {{
  let healthy = 0, sick = 0, total = 0;
  DATA.functions.forEach(l => l.variants.forEach(v => {{
    total++;
    if (v.health === 'healthy') healthy++;
    else if (v.health === 'sick') sick++;
  }}));

  document.getElementById('stats').innerHTML = `
    <div class="stat"><div class="num">${{total}}</div><div class="label">Growing</div></div>
    <div class="stat"><div class="num">${{healthy}}</div><div class="label">In Bloom</div></div>
    <div class="stat sick"><div class="num">${{sick}}</div><div class="label">Withering</div></div>
    <div class="stat dead"><div class="num">${{DATA.obituaries.length}}</div><div class="label">Fallen</div></div>
    <div class="stat runs"><div class="num">${{DATA.run_count}}</div><div class="label">Seasons</div></div>
  `;
}}

function renderGarden() {{
  const el = document.getElementById('garden');
  document.getElementById('func-count').textContent = DATA.functions.length + ' lineages';

  if (DATA.functions.length === 0) {{
    el.innerHTML = '<div class="empty">The soil is untouched. Plant code with <code>grow</code> to begin.</div>';
    return;
  }}

  let html = '';
  DATA.functions.forEach((lineage, i) => {{
    const fittest = lineage.variants.reduce((a, b) => a.fitness > b.fitness ? a : b);
    html += `<div class="plant" style="animation-delay:${{0.1 + i * 0.1}}s">
      <div class="plant-header">
        <div class="plant-icon" style="color:var(--moss-bright)">${{PLANT_SVG}}</div>
        <div class="plant-name">${{lineage.name}}</div>
        <div class="plant-variants-count">${{lineage.variants.length}} ${{lineage.variants.length === 1 ? 'variant' : 'variants'}}</div>
      </div>
      <div class="branches">`;

    lineage.variants.forEach((v, j) => {{
      const isFittest = v.id === fittest.id;
      const lpct = Math.min(100, Math.max(0, v.lifespan));
      const lastUsed = v.last_used === 0 ? 'never' : (DATA.run_count - v.last_used) + ' runs ago';
      const heals = v.healing_attempts > 0 ? `<div class="leaf-stat"><span class="key">heals</span><span class="val">${{v.healing_attempts}}</span></div>` : '';

      html += `<div class="leaf ${{v.health}} ${{isFittest ? 'fittest' : ''}}" style="animation-delay:${{0.2 + i * 0.1 + j * 0.05}}s">
        <div class="leaf-id">${{lineage.name}}<span class="hash">#</span>${{v.id}}</div>
        <div class="leaf-health ${{v.health}}">${{v.health}}</div>
        ${{v.health_detail ? `<div class="leaf-detail">${{v.health_detail}}</div>` : ''}}
        <div class="leaf-stats">
          <div class="leaf-stat"><span class="key">age</span><span class="val">${{v.age}}</span></div>
          <div class="leaf-stat"><span class="key">lifespan</span><span class="val">${{v.lifespan}}</span></div>
          <div class="leaf-stat"><span class="key">usage</span><span class="val">${{v.usage}}</span></div>
          <div class="leaf-stat"><span class="key">fitness</span><span class="val">${{v.fitness}}%</span></div>
          <div class="leaf-stat"><span class="key">trial</span><span class="val">${{v.trial_score}}%</span></div>
          <div class="leaf-stat"><span class="key">last</span><span class="val">${{lastUsed}}</span></div>
          ${{heals}}
        </div>
        <div class="vine"><div class="vine-fill ${{v.health}}" style="width:${{lpct}}%"></div></div>
      </div>`;
    }});

    html += `</div></div>`;
  }});

  el.innerHTML = html;
}}

function renderImmortals() {{
  const el = document.getElementById('immortals');
  const count = DATA.variables.length + DATA.structs.length;
  document.getElementById('immortal-count').textContent = count;

  if (count === 0) {{
    el.innerHTML = '<div class="empty">No stones in the garden.</div>';
    return;
  }}

  let html = '';
  DATA.variables.forEach((name, i) => {{
    html += `<div class="stone" style="animation-delay:${{0.1 + i * 0.05}}s"><span class="glyph">$</span>${{name}}</div>`;
  }});
  DATA.structs.forEach((s, i) => {{
    html += `<div class="stone" style="animation-delay:${{0.1 + i * 0.05}}s"><span class="glyph">S</span>${{s.name}}<span class="fields">{{ ${{s.fields.join(', ')}} }}</span></div>`;
  }});
  el.innerHTML = html;
}}

function renderObituaries() {{
  const el = document.getElementById('obituaries');
  document.getElementById('obit-count').textContent = DATA.obituaries.length;

  if (DATA.obituaries.length === 0) {{
    el.innerHTML = '<div class="empty">No fallen leaves. The garden thrives.</div>';
    return;
  }}

  let html = '';
  DATA.obituaries.forEach((o, i) => {{
    const lastUsed = o.last_used === 0 ? 'never' : (DATA.run_count - o.last_used) + ' runs ago';
    html += `<div class="fallen-leaf ${{o.health}}" style="animation-delay:${{0.05 * i}}s">
      <div class="fallen-name">${{o.name}}<span class="hash">#</span>${{o.id}} <span style="font-size:12px;color:var(--text-dim);font-family:'JetBrains Mono',monospace">— age ${{o.age}}</span></div>
      <div class="fallen-meta">Last used: ${{lastUsed}}</div>
      <div class="fallen-cause">${{o.cause}}</div>
    </div>`;
  }});
  el.innerHTML = html;
}}

renderStats();
renderGarden();
renderImmortals();
renderObituaries();
</script>
</body>
</html>"#);

    let path = std::env::temp_dir().join("veldt_visual.html");
    let mut file = std::fs::File::create(&path)
        .map_err(|e| format!("Failed to create visual: {}", e))?;
    file.write_all(html.as_bytes())
        .map_err(|e| format!("Failed to write visual: {}", e))?;
    Ok(path.to_string_lossy().to_string())
}

pub fn open_in_browser(path: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(&["/c", "start", path]).spawn();
}
