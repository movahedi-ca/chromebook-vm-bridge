// Shared Molson Coors (SuccessFactors classic, career2.successfactors.eu) solver
// Generalized from run 26 (Nov 1, 2026).
// Portal: jobs.molsoncoors.com (jobs2web RMK) -> talentcommunity/apply -> career2.successfactors.eu
// Browser-side: load via `bridge-cli pool eval CTX "$(cat mc-batch.js)"` then call MC.* steps.
// Assumes: headless pool context. Resume PDF pre-chunked to window._r1.._rN (15KB base64 chunks).
//
// KEY PATTERNS (all GENERALIZED):
// 1. DNS: jobs.molsoncoors.com blocked by Control D "Drugs" filter (alcohol) on chromebook-vm
//    -> /etc/hosts entry: 130.214.193.81 jobs.molsoncoors.com (verified via DoH to 1.1.1.1).
//    Same pattern as Aurora (sf-batch.js) - CHECK /etc/hosts FIRST when a sin-industry portal 404s/0.0.0.0.
// 2. Account creation: classic SF register page with Data Privacy Consent Statement dialog:
//    fill fields -> select Country FIRST (dialog requires it) -> openDpcsDialog(true) ->
//    wait for dialog -> click Accept -> IMMEDIATELY click Create Account in the SAME eval
//    (the DPCS acceptance resets on failed submit; fbclc_dpcsId must be fresh).
//    Password policy: 8-18 chars, upper+lower+number/punct (16-char gen used).
//    "Make My Profile Visible to" radios: value 1=worldwide, 0=country, 2=this job only.
// 3. Resume upload: juic multi-attachment widget "51:". Click #51:_attachIcon to open dialog,
//    input id "52:_file" appears. Chunked base64 (15KB) + DataTransfer + change event.
//    TRUNCATION LESSON: verify chunk count on page matches source line count BEFORE dispatch.
//    Uploaded widget id changes after re-render (was 340:). Success = "Download document" text.
//    If upload dialog won't open: widget is in "file attached" state (icon is trash) - delete
//    first, reload page to reset stuck delete spinner.
// 4. Form: juic fields with re-rendered numeric ids (349:_txtFld etc). Map by question text
//    via parent-walk. Dropdowns are rcmpaginatedselectinput: focus+click, then [role=option]
//    by exact text. Resume parsing pre-fills profile/work/education; fix City (parsed Toronto,
//    correct Mississauga) and fill required empties.
// 5. pool eval does NOT await promises: use window._var + poll pattern for AJAX.
// 6. pool click with colon-ids breaks CSS escaping: use getElementById().click() in eval.

globalThis.MC = (function () {
  // PII note: <APPLICANT_*> placeholders must be filled from the operator's
// records before running. Real values are never committed to the repo.
  const STD = {
    firstName: 'Mohammad', middleName: 'Hossein', lastName: 'Movahedi',
    email: 'm.h.movahedi97@gmail.com',
    phone: '<APPLICANT_PHONE>',
    address: '<APPLICANT_ADDRESS>', city: 'Mississauga', postal: 'L5B 0N3',
    country: 'Canada',
  };

  function fire(el) {
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }

  function question(el) {
    let p = el;
    for (let i = 0; i < 12 && p; i++) {
      p = p.parentElement;
      if (!p) break;
      const t = (p.innerText || '').replace(/\s+/g, ' ').trim();
      if (t.length > 8 && t.length < 180) return t.slice(0, 130);
    }
    return '';
  }

  // Map visible text/select inputs to {id, q} by question text
  function mapFields() {
    const out = [];
    document.querySelectorAll('input,select,textarea').forEach(e => {
      if (e.offsetParent === null || /hidden/i.test(e.type)) return;
      if (!e.id) return;
      out.push({ id: e.id, type: e.type || e.tagName, value: (e.value || '').slice(0, 30), q: question(e) });
    });
    return out;
  }

  function setText(id, v) {
    const e = document.getElementById(id);
    if (!e) return false;
    e.focus(); e.value = v; fire(e); e.blur();
    return true;
  }

  // selectInput dropdown: focus+click to open listbox
  function openDropdown(id) {
    const e = document.getElementById(id);
    if (!e) return false;
    e.focus(); e.click();
    return true;
  }

  // click [role=option] by exact visible text
  function pickOption(exactText) {
    const o = [...document.querySelectorAll('[role=option]')]
      .find(x => x.textContent.trim() === exactText);
    if (o) { o.click(); return true; }
    return false;
  }

  // List currently visible options (for discovery)
  function listOptions() {
    return [...document.querySelectorAll('[role=option]')]
      .map(x => x.textContent.trim()).slice(0, 20);
  }

  // Upload resume from pre-chunked window._r1.._rN base64
  function uploadResume(nChunks, fileName) {
    try {
      let b64 = '';
      for (let i = 1; i <= nChunks; i++) b64 += window['_r' + i];
      const bin = atob(b64);
      const arr = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);
      const f = new File([arr], fileName, { type: 'application/pdf' });
      const dt = new DataTransfer();
      dt.items.add(f);
      const inp = document.querySelector('input[type=file]');
      if (!inp) return 'ERR:no-file-input';
      inp.files = dt.files;
      inp.dispatchEvent(new Event('change', { bubbles: true }));
      return 'ok size=' + f.size;
    } catch (e) { return 'ERR:' + e.message; }
  }

  function uploadState() {
    const w = document.querySelector('[id$=":_attachWrapper"]');
    if (!w) return 'no-widget';
    const h = w.innerHTML;
    if (/Download document/.test(h)) return 'uploaded';
    if (/icon_loading/.test(h)) return 'working';
    if (/Upload a Resume/.test(h)) return 'ready';
    return 'other';
  }

  return { STD, question, mapFields, setText, openDropdown, pickOption, listOptions, uploadResume, uploadState };
})();
'MC loaded';
