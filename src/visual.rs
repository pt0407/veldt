// Visual ecosystem viewer — generates an HTML visualization of the veldt

use crate::veldt::Veldt;
use std::io::Write;

pub fn generate_visual(veldt: &Veldt) -> Result<String, String> {
    // Serialize the veldt data as JSON to embed in the HTML
    let data = serde_json::json!({
        "functions": veldt.functions.iter().map(|(name, variants)| {
            serde_json::json!({
                "name": name,
                "variants": variants.iter().map(|e| {
                    let health_str = match &e.health {
                        crate::veldt::Health::Healthy => "healthy",
                        crate::veldt::Health::Sick(_) => "sick",
                        crate::veldt::Health::Dying => "dying",
                    };
                    let health_detail = match &e.health {
                        crate::veldt::Health::Healthy => "".to_string(),
                        crate::veldt::Health::Sick(e) => e.clone(),
                        crate::veldt::Health::Dying => "".into(),
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
        "obituaries": veldt.obituaries.iter().rev().take(20).map(|o| {
            let health_str = match &o.health {
                crate::veldt::Health::Healthy => "healthy",
                crate::veldt::Health::Sick(_) => "sick",
                crate::veldt::Health::Dying => "dying",
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

    let data_json = serde_json::to_string_pretty(&data)
        .map_err(|e| format!("Failed to serialize veldt data: {}", e))?;

    let html = format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Veldt — Ecosystem Viewer</title>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{
    background: #0a0e14;
    color: #c8d4e8;
    font-family: 'SF Mono', 'Fira Code', 'Cascadia Code', monospace;
    min-height: 100vh;
    padding: 24px;
  }}
  h1 {{
    font-size: 28px;
    font-weight: 600;
    margin-bottom: 4px;
    background: linear-gradient(135deg, #7ee787, #58a6ff);
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
  }}
  .subtitle {{ color: #6e7681; font-size: 13px; margin-bottom: 24px; }}
  .container {{ max-width: 1200px; margin: 0 auto; }}
  .section {{
    background: #161b22;
    border: 1px solid #30363d;
    border-radius: 12px;
    padding: 20px;
    margin-bottom: 20px;
  }}
  .section-title {{
    font-size: 16px;
    font-weight: 600;
    margin-bottom: 16px;
    color: #58a6ff;
    display: flex;
    align-items: center;
    gap: 8px;
  }}
  .section-title .count {{
    background: #21262d;
    color: #8b949e;
    font-size: 12px;
    padding: 2px 8px;
    border-radius: 12px;
  }}

  /* Lineage cards */
  .lineage {{
    background: #0d1117;
    border: 1px solid #30363d;
    border-radius: 8px;
    padding: 16px;
    margin-bottom: 12px;
  }}
  .lineage-name {{
    font-size: 15px;
    font-weight: 600;
    color: #e6edf3;
    margin-bottom: 12px;
    display: flex;
    align-items: center;
    gap: 8px;
  }}
  .lineage-name .icon {{ font-size: 18px; }}
  .variants {{ display: flex; gap: 12px; flex-wrap: wrap; }}
  .variant {{
    background: #161b22;
    border: 2px solid #30363d;
    border-radius: 8px;
    padding: 12px 16px;
    min-width: 180px;
    position: relative;
    transition: transform 0.2s, border-color 0.2s;
  }}
  .variant:hover {{ transform: translateY(-2px); }}
  .variant.healthy {{ border-color: #7ee787; }}
  .variant.sick {{ border-color: #f85149; }}
  .variant.dying {{ border-color: #6e7681; opacity: 0.6; }}
  .variant.fittest {{
    box-shadow: 0 0 12px rgba(126, 231, 135, 0.3);
    border-color: #7ee787;
  }}
  .variant.fittest::after {{
    content: '★';
    position: absolute;
    top: -10px;
    right: -10px;
    background: #7ee787;
    color: #0a0e14;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 12px;
    font-weight: bold;
  }}
  .variant-id {{ font-size: 14px; font-weight: 600; margin-bottom: 8px; }}
  .variant-id .hash {{ color: #6e7681; }}
  .variant-health {{
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 10px;
    display: inline-block;
    margin-bottom: 8px;
  }}
  .variant-health.healthy {{ background: rgba(126,231,135,0.15); color: #7ee787; }}
  .variant-health.sick {{ background: rgba(248,81,73,0.15); color: #f85149; }}
  .variant-health.dying {{ background: rgba(110,118,129,0.15); color: #6e7681; }}
  .variant-stat {{
    display: flex;
    justify-content: space-between;
    font-size: 12px;
    color: #8b949e;
    margin-bottom: 4px;
  }}
  .variant-stat .value {{ color: #c8d4e8; font-weight: 600; }}
  .lifespan-bar {{
    height: 4px;
    background: #21262d;
    border-radius: 2px;
    margin-top: 8px;
    overflow: hidden;
  }}
  .lifespan-bar-fill {{
    height: 100%;
    border-radius: 2px;
    transition: width 0.5s;
  }}
  .lifespan-bar-fill.healthy {{ background: #7ee787; }}
  .lifespan-bar-fill.sick {{ background: #f85149; }}
  .lifespan-bar-fill.dying {{ background: #6e7681; }}

  /* Immortal entries */
  .immortal-list {{ display: flex; gap: 12px; flex-wrap: wrap; }}
  .immortal {{
    background: #161b22;
    border: 1px solid #30363d;
    border-radius: 8px;
    padding: 10px 16px;
    font-size: 13px;
  }}
  .immortal .icon {{ margin-right: 6px; }}
  .immortal .type {{ color: #8b949e; font-size: 11px; }}

  /* Obituaries */
  .obituary {{
    background: #0d1117;
    border-left: 3px solid #6e7681;
    border-radius: 0 8px 8px 0;
    padding: 12px 16px;
    margin-bottom: 8px;
    font-size: 13px;
  }}
  .obituary.healthy {{ border-left-color: #7ee787; }}
  .obituary.sick {{ border-left-color: #f85149; }}
  .obituary-name {{ font-weight: 600; color: #e6edf3; }}
  .obituary-detail {{ color: #8b949e; margin-top: 4px; }}
  .obituary-cause {{ color: #d29922; margin-top: 4px; }}

  /* Empty state */
  .empty {{
    text-align: center;
    padding: 40px;
    color: #6e7681;
    font-size: 14px;
  }}

  /* Stats bar */
  .stats {{
    display: flex;
    gap: 16px;
    margin-bottom: 20px;
    flex-wrap: wrap;
  }}
  .stat {{
    background: #161b22;
    border: 1px solid #30363d;
    border-radius: 8px;
    padding: 12px 20px;
    text-align: center;
  }}
  .stat .num {{ font-size: 24px; font-weight: 700; color: #58a6ff; }}
  .stat .label {{ font-size: 11px; color: #6e7681; text-transform: uppercase; }}
  .stat.healthy .num {{ color: #7ee787; }}
  .stat.sick .num {{ color: #f85149; }}
  .stat.dead .num {{ color: #6e7681; }}
</style>
</head>
<body>
<div class="container">
  <h1>Veldt Ecosystem</h1>
  <p class="subtitle">A living codebase — functions compete, evolve, and die by natural selection</p>

  <div class="stats" id="stats"></div>

  <div class="section" id="functions-section">
    <div class="section-title">
      Living Functions <span class="count" id="func-count"></span>
    </div>
    <div id="functions"></div>
  </div>

  <div class="section" id="immortals-section">
    <div class="section-title">
      Immortal Data <span class="count" id="immortal-count"></span>
    </div>
    <div id="immortals"></div>
  </div>

  <div class="section" id="obituaries-section">
    <div class="section-title">
      Obituaries <span class="count" id="obit-count"></span>
    </div>
    <div id="obituaries"></div>
  </div>
</div>

<script>
const DATA = {data_json};

function renderStats() {{
  let healthy = 0, sick = 0, total = 0;
  DATA.functions.forEach(lineage => {{
    lineage.variants.forEach(v => {{
      total++;
      if (v.health === 'healthy') healthy++;
      else if (v.health === 'sick') sick++;
    }});
  }});
  const dead = DATA.obituaries.length;

  document.getElementById('stats').innerHTML = `
    <div class="stat"><div class="num">${{total}}</div><div class="label">Living Variants</div></div>
    <div class="stat healthy"><div class="num">${{healthy}}</div><div class="label">Healthy</div></div>
    <div class="stat sick"><div class="num">${{sick}}</div><div class="label">Sick</div></div>
    <div class="stat dead"><div class="num">${{dead}}</div><div class="label">Total Deaths</div></div>
    <div class="stat"><div class="num">${{DATA.run_count}}</div><div class="label">Runs</div></div>
  `;
}}

function renderFunctions() {{
  const container = document.getElementById('functions');
  document.getElementById('func-count').textContent = DATA.functions.length + ' lineages';

  if (DATA.functions.length === 0) {{
    container.innerHTML = '<div class="empty">The veldt is empty. Run a program with <code>grow</code> statements to plant code.</div>';
    return;
  }}

  // Find fittest per lineage
  let html = '';
  DATA.functions.forEach(lineage => {{
    const fittest = lineage.variants.reduce((a, b) => a.fitness > b.fitness ? a : b);
    html += `<div class="lineage">
      <div class="lineage-name"><span class="icon">fn</span> ${{lineage.name}} <span style="color:#6e7681;font-size:12px">(${{lineage.variants.length}} variant${{lineage.variants.length > 1 ? 's' : ''}})</span></div>
      <div class="variants">`;
    lineage.variants.forEach(v => {{
      const isFittest = v.id === fittest.id;
      const lifespanPct = Math.min(100, (v.lifespan / 100 * 100));
      const lastUsed = v.last_used === 0 ? 'never' : (DATA.run_count - v.last_used) + ' runs ago';
      html += `<div class="variant ${{v.health}} ${{isFittest ? 'fittest' : ''}}">
        <div class="variant-id">${{lineage.name}}<span class="hash">#</span>${{v.id}}</div>
        <div class="variant-health ${{v.health}}">${{v.health}}${{v.health_detail ? ': ' + v.health_detail : ''}}</div>
        <div class="variant-stat"><span>age</span><span class="value">${{v.age}}</span></div>
        <div class="variant-stat"><span>lifespan</span><span class="value">${{v.lifespan}}</span></div>
        <div class="variant-stat"><span>usage</span><span class="value">${{v.usage}}</span></div>
        <div class="variant-stat"><span>fitness</span><span class="value">${{v.fitness}}%</span></div>
        <div class="variant-stat"><span>trial</span><span class="value">${{v.trial_score}}%</span></div>
        <div class="variant-stat"><span>last used</span><span class="value">${{lastUsed}}</span></div>
        ${{v.healing_attempts > 0 ? `<div class="variant-stat"><span>heals</span><span class="value">${{v.healing_attempts}}</span></div>` : ''}}
        <div class="lifespan-bar"><div class="lifespan-bar-fill ${{v.health}}" style="width:${{lifespanPct}}%"></div></div>
      </div>`;
    }});
    html += `</div></div>`;
  }});
  container.innerHTML = html;
}}

function renderImmortals() {{
  const container = document.getElementById('immortals');
  const count = DATA.variables.length + DATA.structs.length;
  document.getElementById('immortal-count').textContent = count;

  if (count === 0) {{
    container.innerHTML = '<div class="empty">No immortal data yet.</div>';
    return;
  }}

  let html = '<div class="immortal-list">';
  DATA.variables.forEach(name => {{
    html += `<div class="immortal"><span class="icon">$</span>${{name}} <span class="type">variable</span></div>`;
  }});
  DATA.structs.forEach(s => {{
    html += `<div class="immortal"><span class="icon">struct</span>${{s.name}} <span class="type">{{ ${{s.fields.join(', ')}} }}</span></div>`;
  }});
  html += '</div>';
  container.innerHTML = html;
}}

function renderObituaries() {{
  const container = document.getElementById('obituaries');
  document.getElementById('obit-count').textContent = DATA.obituaries.length;

  if (DATA.obituaries.length === 0) {{
    container.innerHTML = '<div class="empty">No deaths recorded. The ecosystem is thriving.</div>';
    return;
  }}

  let html = '';
  DATA.obituaries.forEach(o => {{
    const lastUsed = o.last_used === 0 ? 'never' : (DATA.run_count - o.last_used) + ' runs ago';
    html += `<div class="obituary ${{o.health}}">
      <div class="obituary-name">[OBITUARY] ${{o.name}}#${{o.id}} — died at age ${{o.age}} (${{o.health}})</div>
      <div class="obituary-detail">Last used: ${{lastUsed}}</div>
      <div class="obituary-cause">Cause: ${{o.cause}}</div>
    </div>`;
  }});
  container.innerHTML = html;
}}

renderStats();
renderFunctions();
renderImmortals();
renderObituaries();
</script>
</body>
</html>"#);

    // Write to temp file
    let path = std::env::temp_dir().join("veldt_visual.html");
    let mut file = std::fs::File::create(&path)
        .map_err(|e| format!("Failed to create visual file: {}", e))?;
    file.write_all(html.as_bytes())
        .map_err(|e| format!("Failed to write visual file: {}", e))?;

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
