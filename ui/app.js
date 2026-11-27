// Popover logic.
//
// Reset-time formatting lives here rather than in Rust: Intl.DateTimeFormat
// carries full locale data for free, where Rust would need the heavy `icu`
// crate. This is the JS counterpart of SharedUsage.swift's resetString.

// The UI is buildless, so it reaches Tauri through the window global rather
// than an npm import. That global only exists when `withGlobalTauri` is set in
// tauri.conf.json — without it this file throws on its first line and the
// popover renders as an empty window, which is exactly what it looked like.
// Windows-only styling hook (see `.platform-windows` in style.css). Set first
// so it applies even if the Tauri global below is missing.
if (navigator.userAgent.includes("Windows")) {
  document.documentElement.classList.add("platform-windows");
}

// Tray layouts differ per platform (see `supported` in tray/mod.rs): drop the
// options this one cannot show, and name the tray the way the OS does.
{
  const ua = navigator.userAgent;
  const platform = ua.includes("Windows") ? "windows" : ua.includes("Mac") ? "macos" : "linux";
  for (const option of document.querySelectorAll("#tray-display option[data-platforms]")) {
    if (!option.dataset.platforms.split(" ").includes(platform)) option.remove();
  }
  const label = document.getElementById("tray-display-label");
  if (label && platform === "windows") label.textContent = "Taskbar shows";
}

const tauri = window.__TAURI__;
if (!tauri) {
  document.body.innerHTML =
    '<p style="padding:16px;font:13px system-ui">Tauri API unavailable — ' +
    'set <code>app.withGlobalTauri</code> in tauri.conf.json.</p>';
  throw new Error("window.__TAURI__ is undefined");
}
const { invoke } = tauri.core;
const { listen } = tauri.event;
const { getCurrentWindow, LogicalSize } = tauri.window;

// Thresholds must match Theme.swift / UsageLevel::for_percent.
function usageColor(percent) {
  if (percent < 70) return "var(--usage-green)";
  if (percent < 90) return "var(--usage-amber)";
  return "var(--usage-red)";
}

// "resets in 3h 44m (9:42 PM)".
//
// The 5-hour window always resets today or tonight, so the clock time alone is
// unambiguous. The 7-day window can land on any date, so it also needs the
// weekday and date — the same two styles SharedUsage.swift used.
function resetString(epochMs, style) {
  if (epochMs == null) return "—";

  const target = new Date(epochMs);
  const diffMs = epochMs - Date.now();
  if (diffMs <= 0) return "resetting…";

  const totalMinutes = Math.floor(diffMs / 60000);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  const countdown = hours > 0 ? `resets in ${hours}h ${minutes}m` : `resets in ${minutes}m`;

  const options =
    style === "dateAndTime"
      ? { weekday: "short", day: "numeric", month: "short", hour: "numeric", minute: "2-digit" }
      : { hour: "numeric", minute: "2-digit" };

  // Undefined locale = the system locale, so 12- vs 24-hour follows the OS.
  return `${countdown} (${new Intl.DateTimeFormat(undefined, options).format(target)})`;
}

/// Poll interval in seconds, kept in sync with the settings panel so the
/// countdown stays right when the user changes it.
let pollIntervalSecs = 60;

/// "updated 12s ago · next in 48s".
///
/// The countdown is derived from lastUpdated plus the poll interval rather
/// than tracked separately: the poller has no callback into the UI, and a
/// separately-run timer would drift out of step with it after a failed poll
/// or a settings change.
function footerText(snapshot) {
  if (snapshot.lastUpdatedMs == null) return snapshot.isStale ? "offline — retrying" : "—";
  // Only once the reading is genuinely old, matching the banner. A failed
  // poll backs off, so the next attempt is off the normal cadence and
  // promising a time would be wrong.
  if (snapshot.isStale && isReadingOld(snapshot)) return "offline — retrying";

  const updated = `updated ${relativeTime(snapshot.lastUpdatedMs)}`;
  const dueMs = snapshot.lastUpdatedMs + pollIntervalSecs * 1000;
  const remaining = Math.round((dueMs - Date.now()) / 1000);

  // A poll can overrun its slot; "now" is honest where a negative is not.
  if (remaining <= 0) return `${updated} · refreshing…`;
  // Seconds past a minute too: at the default 60s interval a fresh poll would
  // otherwise read "next in 1m" and sit there, looking stuck.
  if (remaining < 90) return `${updated} · next in ${remaining}s`;
  return `${updated} · next in ${Math.round(remaining / 60)}m`;
}

function relativeTime(epochMs) {
  if (epochMs == null) return "—";
  const seconds = Math.floor((Date.now() - epochMs) / 1000);
  if (seconds < 10) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  return `${Math.floor(minutes / 60)}h ago`;
}

const el = (id) => document.getElementById(id);

let latest = null;

function render(snapshot) {
  latest = snapshot;

  const authed = snapshot.isAuthenticated;
  el("no-credential").hidden = authed;
  el("panel").hidden = !authed;
  if (!authed) return;

  el("plan").textContent = snapshot.plan ?? "";

  // A stale poll means the reading could not be refreshed — not that it is
  // meaningless. Keep showing the last known numbers rather than blanking
  // them: at 100% usage the last reading is the one that explains why
  // everything else stopped working. The notice below says it is not fresh.
  const stale = snapshot.isStale;
  const hasReading = snapshot.lastUpdatedMs != null;

  for (const [key, style] of [
    ["session", "timeOnly"],
    ["weekly", "dateAndTime"],
  ]) {
    const percent = snapshot[`${key}Percent`];

    const value = el(`${key}-value`);
    value.textContent = hasReading ? `${Math.round(percent)}%` : "—";
    // The number carries the threshold colour too, not just the bar — that is
    // how the Swift original read at a glance.
    value.style.color = hasReading ? usageColor(percent) : "var(--fg-muted)";

    const bar = el(`${key}-bar`);
    bar.style.width = hasReading ? `${Math.min(100, Math.max(0, percent))}%` : "0%";
    bar.style.backgroundColor = usageColor(percent);

    // The reset time stays true while offline — the window keeps ticking
    // whether or not the app can reach the API.
    el(`${key}-reset`).textContent = hasReading
      ? resetString(snapshot[`${key}ResetAtMs`], style)
      : "—";
  }

  renderNotice(snapshot, stale, hasReading);

  el("cred-source").textContent = snapshot.credentialSource ?? "—";
  el("app-version").textContent = snapshot.version ?? "—";
  // Also in the header, where it is visible without expanding Settings.
  el("header-version").textContent = snapshot.version ? `v${snapshot.version}` : "";

  renderStatus(snapshot);
  el("last-updated").textContent = footerText(snapshot);
}

/// Whether the last good reading is old enough to be worth warning about.
///
/// Two missed polls, so one transient failure that the next poll recovers
/// never surfaces a banner. With no reading at all there is nothing to age,
/// and the warning is always warranted.
function isReadingOld(snapshot) {
  if (snapshot.lastUpdatedMs == null) return true;
  return Date.now() - snapshot.lastUpdatedMs > pollIntervalSecs * 2000;
}

/// The banner above the metrics. Separates "your quota is spent" from "the
/// app cannot reach the API" — identical from the numbers alone, very
/// different in what they mean.
function renderNotice(snapshot, stale, hasReading) {
  const notice = el("notice");
  const icon = el("notice-icon");
  const text = el("notice-text");

  if (snapshot.rateLimited) {
    notice.hidden = false;
    notice.className = "notice limit";
    icon.textContent = "\u26D4";
    const resets = resetString(snapshot.sessionResetAtMs, "timeOnly");
    text.textContent =
      resets === "—"
        ? "Session limit reached — requests are being refused."
        : `Session limit reached — ${resets}.`;
    return;
  }

  // Only complain once the reading is genuinely old. `isStale` flips on the
  // first failed attempt, but the status poller republishes the snapshot on
  // its own 5-minute cadence, so a single blip would otherwise leave the
  // banner up next to a footer reading "updated just now".
  if (stale && isReadingOld(snapshot)) {
    notice.hidden = false;
    notice.className = "notice offline";
    icon.textContent = "\u26A0\uFE0F";
    text.textContent = hasReading
      ? "Can't reach the API — showing the last reading."
      : "Can't reach the API.";
    return;
  }

  notice.hidden = true;
}

// Worst-of summary, mirroring core's overall_health severity ordering.
function renderStatus(snapshot) {
  const services = snapshot.services ?? [];
  const list = el("status-list");
  list.innerHTML = "";

  const labels = {
    operational: "Operational",
    degraded: "Degraded",
    partialOutage: "Partial outage",
    majorOutage: "Major outage",
    maintenance: "Maintenance",
    unknown: "Unknown",
  };

  for (const service of services) {
    const li = document.createElement("li");
    const name = document.createElement("span");
    name.textContent = service.name;
    const health = document.createElement("span");
    health.textContent = labels[service.health] ?? service.health;
    li.append(name, health);
    list.append(li);
  }

  const severity = {
    operational: 0,
    unknown: 1,
    maintenance: 2,
    degraded: 3,
    partialOutage: 4,
    majorOutage: 5,
  };
  const worst = services.reduce(
    (acc, s) => ((severity[s.health] ?? 1) > (severity[acc] ?? 1) ? s.health : acc),
    services.length ? "operational" : "unknown"
  );

  // Name every service that is not green, so the collapsed row says what is
  // wrong without being opened. Worst first; a service at a lesser level than
  // the headline carries its own label, or the headline would overstate it.
  const affected = services
    .filter((s) => s.health !== "operational")
    .sort((a, b) => (severity[b.health] ?? 1) - (severity[a.health] ?? 1))
    .map((s) =>
      s.health === worst
        ? s.name
        : `${s.name}: ${(labels[s.health] ?? s.health).toLowerCase()}`
    );

  el("status-dot").className = `status-dot ${worst}`;
  const headline = labels[worst] ?? "Service status";
  el("status-summary").textContent =
    worst === "operational"
      ? "All systems operational"
      : affected.length
        ? `${headline} (${affected.join(", ")})`
        : headline;
}

// MARK: - History

const HOUR_MS = 3_600_000;
// Mirrors history::HEARTBEAT_SECS. The backend writes at least this often
// while polling, so a longer silence (plus a poll's slack) means no reading
// was taken, and the line breaks there instead of bridging the gap.
const HEARTBEAT_MS = 30 * 60 * 1000;
const SPARK_W = 248;
const SPARK_H = 30;
const SVG_NS = "http://www.w3.org/2000/svg";

/// Fetched only while the section is open. Closed, it costs no IPC and no
/// drawing, which is the state the popover spends nearly all its time in.
async function loadHistory() {
  if (!el("history").open) return;
  const now = Date.now();
  let samples;
  try {
    // One fetch covers both charts: the weekly span contains the session one.
    samples = await invoke("get_history", { sinceMs: now - 7 * 24 * HOUR_MS });
  } catch {
    return;
  }
  drawSpark("session", samples, 1, now - 24 * HOUR_MS, now);
  drawSpark("weekly", samples, 2, now - 7 * 24 * HOUR_MS, now);
}

/// Draws one sparkline from `[epochMs, session, weekly]` triples.
///
/// A step line, not a diagonal: usage holds its value until the next reading,
/// and a slope between two readings would invent values that never existed.
function drawSpark(key, samples, column, from, to) {
  // Start from the reading that held at this chart's left edge.
  let start = 0;
  while (start + 1 < samples.length && samples[start + 1][0] <= from) start++;
  const points = samples.slice(start);

  const pollMs = pollIntervalSecs * 1000;
  const gapMs = HEARTBEAT_MS + 2 * pollMs;
  const x = (t) => (((Math.min(Math.max(t, from), to) - from) / (to - from)) * SPARK_W).toFixed(1);
  const y = (v) => (SPARK_H - 1 - (Math.min(100, Math.max(0, v)) / 100) * (SPARK_H - 2)).toFixed(1);

  let line = "";
  let area = "";
  let run = "";
  let runStartX = null;
  for (let i = 0; i < points.length; i++) {
    const t = points[i][0];
    const value = points[i][column];
    const next = points[i + 1];

    if (runStartX == null) {
      runStartX = x(t);
      run = `M${runStartX} ${y(value)}`;
    } else {
      run += `V${y(value)}`;
    }

    // Hold until the next reading. At a gap, or the newest reading when it is
    // not current, hold for one poll only — that much is known to be true.
    const endsRun = !next || next[0] - t > gapMs;
    const until = !endsRun ? next[0] : !next && to - t <= gapMs ? to : t + pollMs;
    run += `H${x(until)}`;

    if (endsRun) {
      line += run;
      area += `${run}V${SPARK_H}H${runStartX}Z`;
      runStartX = null;
    }
  }

  const color = points.length ? usageColor(points[points.length - 1][column]) : "var(--fg-muted)";
  el(`${key}-spark`).replaceChildren(
    svgPath("spark-area", area, { fill: color }),
    svgPath("spark-line", line, { stroke: color })
  );
  el(`${key}-peak`).textContent = points.length
    ? `peak ${Math.max(...points.map((p) => p[column]))}%`
    : "no readings yet";
}

function svgPath(className, d, style) {
  const path = document.createElementNS(SVG_NS, "path");
  path.setAttribute("class", className);
  path.setAttribute("d", d);
  Object.assign(path.style, style);
  return path;
}

el("history").addEventListener("toggle", loadHistory);
// The popover is hidden rather than closed, so a section left open must catch
// up when the window comes back.
window.addEventListener("focus", loadHistory);

// MARK: - Settings

// Applied optimistically, then reconciled with what the backend actually
// stored — it clamps out-of-range values, so the UI must not assume its own
// input survived unchanged.
async function loadSettings() {
  try {
    const s = await invoke("get_settings");
    pollIntervalSecs = s.refreshInterval;
    el("interval").value = String(s.refreshInterval);
    el("tray-display").value = s.trayDisplay;
    el("alert80").checked = s.alert80;
    el("alert95").checked = s.alert95;
  } catch {
    // Settings are a convenience; the panel still works without them.
  }
}

async function saveSettings() {
  try {
    const saved = await invoke("set_settings", {
      settings: {
        refreshInterval: Number(el("interval").value),
        trayDisplay: el("tray-display").value,
        alert80: el("alert80").checked,
        alert95: el("alert95").checked,
      },
    });
    // Reflect any clamping back into the controls.
    pollIntervalSecs = saved.refreshInterval;
    el("interval").value = String(saved.refreshInterval);
    if (latest) render(latest);
  } catch {
    // Leave the controls as the user set them; the change just did not persist.
  }
}

for (const id of ["interval", "tray-display", "alert80", "alert95"]) {
  el(id).addEventListener("change", saveSettings);
}

// Launch at login lives outside saveSettings on purpose: the OS registration
// is the truth, not settings.json, so the checkbox always shows what the
// backend reports back rather than what was clicked.
function renderLoginItem(item, error) {
  const box = el("login-item");
  const note = el("login-item-note");
  box.checked = item.enabled;
  box.disabled = item.unavailableReason != null && !item.enabled;
  const message = error ?? item.unavailableReason;
  note.textContent = message ?? "";
  note.hidden = message == null;
}

async function loadLoginItem() {
  try {
    renderLoginItem(await invoke("get_login_item"));
  } catch {
    el("login-item").disabled = true;
  }
}

el("login-item").addEventListener("change", async (event) => {
  const wanted = event.target.checked;
  try {
    renderLoginItem(await invoke("set_login_item", { enabled: wanted }));
  } catch (error) {
    // Re-read rather than guess: a failed enable may have left nothing, a
    // failed disable may have left the old registration in place.
    try {
      renderLoginItem(await invoke("get_login_item"), String(error));
    } catch {
      el("login-item").checked = !wanted;
    }
  }
});

el("test-notification").addEventListener("click", () => invoke("send_test_notification"));

el("refresh").addEventListener("click", () => invoke("refresh_now"));
el("recheck").addEventListener("click", () => invoke("recheck_credentials"));

// Quit is in the tray's right-click menu too, but that is not discoverable —
// so both popover states carry a button.
for (const id of ["quit", "quit-nc"]) {
  el(id).addEventListener("click", () => invoke("quit_app"));
}

listen("usage://snapshot", (event) => {
  render(event.payload);
  // Only while someone is looking; a hidden popover need not keep up.
  if (document.hasFocus()) loadHistory();
});
invoke("get_snapshot").then(render);
loadSettings();
loadLoginItem();
// Re-read on every expand: the login item can be removed from System Settings
// (or a startup manager) while the app runs, and the popover is long-lived.
el("settings").addEventListener("toggle", () => el("settings").open && loadLoginItem());

// Shrink the window to whatever is actually rendered. The panel's height
// varies — the status accordion expands, and the no-credential state is much
// shorter than the usage panel — so a fixed height would leave dead space
// below the content.
let lastHeight = 0;
async function fitWindow() {
  const visible = document.querySelector("section:not([hidden])");
  if (!visible) return;
  // The section's own rect already includes its padding; what it misses is
  // the body's 1px transparent gutter that keeps the rounded corners off the
  // window edge.
  const style = getComputedStyle(document.body);
  const padding = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
  const height = Math.ceil(visible.getBoundingClientRect().height + padding);
  if (height > 0 && Math.abs(height - lastHeight) > 1) {
    lastHeight = height;
    try {
      // 282 = the 280pt panel plus the body's 1px gutter on each side.
      await getCurrentWindow().setSize(new LogicalSize(282, height));
    } catch {
      // Resizing is a nicety; a denied permission must not break the panel.
    }
  }
}

// Re-fit after any render, and when the status accordion is toggled.
const fitSoon = () => requestAnimationFrame(fitWindow);
new MutationObserver(fitSoon).observe(document.body, {
  subtree: true,
  childList: true,
  attributes: true,
});
document.querySelector(".status")?.addEventListener("toggle", fitSoon);

// The reset countdowns move slowly; a full redraw every 30s is enough.
setInterval(() => latest && render(latest), 30000);

// The next-poll countdown ticks every second, and only touches one node so it
// does not fight the window auto-fit.
setInterval(() => {
  if (latest && latest.isAuthenticated) el("last-updated").textContent = footerText(latest);
}, 1000);
