// Shared SAP SuccessFactors application solver for sin-industry program
// Generalized from Aurora Cannabis runs 06-10 (Nov 1-2, 2026)
// Portal: career17.sapsf.com (company=auroracann); RMK site careers.auroramj.com
// Timing: 83-106s per full application (runs 07-10)
//
// KEY PATTERNS (all GENERALIZED):
// 1. Pool (headless) contexts for ALL work; never touch the shared visible browser
// 2. IDs re-render per page load: map fields by QUESTION TEXT via parent-walk, never hardcode IDs
// 3. Sections are collapsed accordions: click "Expand all sections" first
// 4. selectInput dropdowns: focus+click input, then click [role=option] by EXACT text
// 5. Resume upload: chunked base64 (4x16KB via window._c1.._c4) + DataTransfer + dispatched
//    change event; the juic inline onchange fires on synthetic events; component re-renders
//    to "File is uploaded successfully" (input disappears = success, verify via attachWrapper HTML)
// 6. pool eval does NOT await promises: use window._var + poll pattern
// 7. Pool page cannot fetch http://localhost: use chunked base64 instead
// 8. Radios are span.radioCheck: click by ID after mapping via question text
// 9. Apply button: span[id$=_submitBtn] found via lastIndexOf('>Apply<') in HTML
// 10. Confirmation: page title "Successfully Applied to <role> (<req>)"; verify via
//     My Profile > Jobs Applied (N) list
//
// DNS: careers.auroramj.com blocked by Control D on VM -> /etc/hosts:
//   20.200.113.11 careers.auroramj.com
// (bare auroramj.jobs2web.com 301s to sap.com; correct SNI serves the site)

// Standard honest answers (per GOAL.md)
const SF_STD = {
  howHeard: 'Company Website',
  travel: '0 - 10%',
  authorized: 'Yes',
  sponsorship: 'No',
  entitledCA: null, // radio ID mapped per page
  genderMale: null, // radio ID mapped per page
  over18: null,
  salary: 'Negotiable',
  signature: 'Mohammad Movahedi',
  referred: 'No',
  related: 'No',
  address: '36 Elm Dr W, Unit 3203',
  city: 'Mississauga',
  postal: 'L5B 0N3',
  email: 'm.h.movahedi97@gmail.com',
};

// Find nearest question text for an element (walk up parents)
function sfQuestion(el, maxLen) {
  maxLen = maxLen || 200;
  let p = el;
  for (let i = 0; i < 16 && p; i++) {
    p = p.parentElement;
    if (!p) break;
    const t = (p.innerText || '').replace(/\s+/g, ' ').trim();
    if (t.length > 12 && t.length < maxLen) return t.slice(0, 130);
  }
  return '';
}

// Map visible _input dropdowns to {id, question}, excluding profile/work/education rows
function sfMapDropdowns() {
  const out = [];
  document.querySelectorAll('input').forEach(e => {
    if (e.offsetParent === null || !/_input$/.test(e.id)) return;
    const q = sfQuestion(e);
    if (q && !/First Name|Company Name|Present Employer|Type of Business|School Degree|State \/ Province/.test(q))
      out.push({ id: e.id, q });
  });
  return out;
}

// Select a selectInput dropdown option by exact text (two-phase: open, then pick)
function sfOpenDropdown(id) {
  const e = document.getElementById(id);
  if (e) { e.focus(); e.click(); }
}
function sfPickOption(exactText) {
  const o = [...document.querySelectorAll('[role=option]')]
    .find(x => x.textContent.trim() === exactText);
  if (o) { o.click(); return true; }
  return false;
}

// Fill the 4 standard dropdowns by question text
function sfFillStandardDropdowns() {
  const map = {};
  sfMapDropdowns().forEach(d => {
    if (/how did you hear/i.test(d.q)) map.heard = d.id;
    else if (/willingness to travel/i.test(d.q)) map.travel = d.id;
    else if (/legally authorized/i.test(d.q)) map.auth = d.id;
    else if (/sponsorship/i.test(d.q)) map.spons = d.id;
  });
  return map; // caller: sfOpenDropdown(id) -> sleep -> sfPickOption(text), chained
}

// Set a text field / textarea by ID with proper events
function sfSet(id, v) {
  const e = document.getElementById(id);
  if (!e) return false;
  e.focus(); e.value = v;
  e.dispatchEvent(new Event('input', { bubbles: true }));
  e.dispatchEvent(new Event('change', { bubbles: true }));
  e.blur();
  return true;
}

// Click a radio span by ID
function sfRadio(id) {
  const e = document.getElementById(id);
  if (e) { e.click(); return true; }
  return false;
}

// Find and click the Apply submit button
function sfClickApply() {
  const html = document.documentElement.innerHTML;
  const i = html.lastIndexOf('>Apply<');
  const seg = html.slice(Math.max(0, i - 500), i);
  const m = seg.match(/id="([0-9]+:_submitBtn)"/);
  if (m) {
    const b = document.getElementById(m[1]);
    if (b) { b.click(); return true; }
  }
  return false;
}
