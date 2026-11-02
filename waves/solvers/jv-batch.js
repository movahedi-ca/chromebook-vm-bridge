// Shared Jobvite application solver for sin-industry program
// Generalized from Cronos Group run 11 (Nov 1, 2026). Timing: 169s first run.
// Browser-side: load via `bridge-cli pool eval CTX <file>` then call JV.* steps.
// Assumes: headless pool context (visible Chrome is SHARED with other workers - never touch it).

globalThis.JV = (function () {
  // Standard answers (honest, per GOAL.md)
  // PII note: <APPLICANT_*> placeholders must be filled from the operator's
// records before running. Real values are never committed to the repo.
  const STD = {
    firstName: 'Mohammad',
    lastName: 'Movahedi',
    address: '<APPLICANT_ADDRESS>',
    email: 'm.h.movahedi97@gmail.com',
    phone: '<APPLICANT_PHONE_DIGITS>', // digits-only (Workday lesson; Jobvite accepts it too)
    city: 'Mississauga',
    province: 'Ontario',
    country: 'Canada',
    salary: '120000', // his honest floor, CAD
    education: 'Masters',
    yes: 'Yes', no: 'No',
  };

  function fire(el) {
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }

  function labelFor(el) {
    if (el.id) {
      const l = document.querySelector('label[for="' + el.id + '"]');
      if (l) return l.textContent;
    }
    // fallback: nearest preceding label-ish sibling in the field container
    let p = el.parentElement;
    for (let d = 0; d < 4 && p; d++) {
      const l = p.querySelector('label, .jv-field-label, span.jv-label');
      if (l && l.textContent.trim().length > 1 && l.textContent.trim().length < 80)
        return l.textContent;
      p = p.parentElement;
    }
    return '';
  }

  function norm(s) { return (s || '').toLowerCase().replace(/[^a-z ]/g, '').trim(); }

  // --- Step 0: resume upload via DataTransfer (bridge /browser/upload only hits default ctx) ---
  function uploadResume(b64, filename) {
    filename = filename || 'Movahedi-Resume-Standard.pdf';
    const bytes = atob(b64);
    const arr = new Uint8Array(bytes.length);
    for (let i = 0; i < bytes.length; i++) arr[i] = bytes.charCodeAt(i);
    const file = new File([new Blob([arr], { type: 'application/pdf' })], filename, { type: 'application/pdf' });
    const input = document.querySelector('input[type=file]');
    if (!input) return 'NO-FILE-INPUT';
    const dt = new DataTransfer();
    dt.items.add(file);
    input.files = dt.files;
    input.dispatchEvent(new Event('change', { bubbles: true }));
    return 'files:' + input.files.length;
  }

  // --- Step 1: personal info, discovered by label text (IDs are per-job generated) ---
  function fillPersonal() {
    const done = [];
    document.querySelectorAll('input[type=text], input[type=email], input[type=tel], input:not([type])').forEach(function (el) {
      if (el.type === 'file' || el.type === 'hidden') return;
      const lab = norm(labelFor(el));
      let v = null;
      if (/first.name/.test(lab)) v = STD.firstName;
      else if (/last.name/.test(lab)) v = STD.lastName;
      else if (/email/.test(lab)) v = STD.email;
      else if (/primary.phone|^phone|mobile/.test(lab)) v = STD.phone;
      else if (/address/.test(lab)) v = STD.address;
      else if (/city/.test(lab)) v = STD.city;
      if (v !== null && !el.value) { el.value = v; fire(el); done.push(lab + '=' + v); }
    });
    document.querySelectorAll('select').forEach(function (el) {
      const lab = norm(labelFor(el));
      let want = null;
      if (/province/.test(lab)) want = STD.province;
      else if (/country/.test(lab)) want = STD.country;
      if (!want) return;
      const opt = Array.from(el.options).find(function (o) { return norm(o.text) === norm(want); });
      if (opt && el.value !== opt.value) { el.value = opt.value; fire(el); done.push(lab + '=' + want); }
    });
    return JSON.stringify(done);
  }

  // --- Step 2: general questions (US and Canada Reqs), discovered by label text ---
  function fillQuestions() {
    const done = [];
    const pick = function (el, want) {
      const opt = Array.from(el.options).find(function (o) { return norm(o.text) === norm(want); });
      if (opt && el.value !== opt.value) { el.value = opt.value; fire(el); return true; }
      return false;
    };
    document.querySelectorAll('input[type=text], textarea').forEach(function (el) {
      if (el.type === 'file' || el.id === 'g-recaptcha-response') return;
      const lab = norm(labelFor(el));
      if (/salary/.test(lab) && !el.value) { el.value = STD.salary; fire(el); done.push('salary=' + STD.salary); }
    });
    document.querySelectorAll('select').forEach(function (el) {
      const lab = norm(labelFor(el));
      let want = null;
      if (/know anyone|referral/.test(lab)) want = STD.no;
      else if (/highest level of education|education.*achieved/.test(lab)) want = STD.education;
      else if (/legally authorized|authorized to work|eligible to work/.test(lab)) want = STD.yes;
      else if (/ever worked|previously (worked|employed)/.test(lab)) want = STD.no;
      else if (/require.*sponsor|sponsorship/.test(lab)) want = STD.no;
      else if (/criminal|conviction|felony/.test(lab)) want = STD.no;
      else if (/how did you hear|how did you learn/.test(lab)) want = 'Corporate Website';
      if (want && pick(el, want)) done.push(lab.slice(0, 30) + '=' + want);
    });
    return JSON.stringify(done);
  }

  function clickNext() {
    const bs = document.querySelectorAll('button');
    for (let i = 0; i < bs.length; i++) {
      if (/next/i.test(bs[i].textContent) && !bs[i].disabled) { bs[i].click(); return 'clicked-next'; }
    }
    return 'no-next';
  }

  function send() {
    const out = [];
    document.querySelectorAll('button[type=submit], button').forEach(function (b) {
      if (/send application/i.test(b.textContent) && !b.disabled) { b.click(); out.push('clicked-send'); }
    });
    return JSON.stringify(out.length ? out : ['no-send-enabled']);
  }

  function confirmation() {
    return JSON.stringify({
      url: location.href,
      sent: document.body.innerText.indexOf('Application Sent!') > -1 || document.body.innerText.indexOf('Your application has been received!') > -1 || location.href.indexOf('applyConfirmation') > -1,
      head: document.body.innerText.slice(0, 300),
    });
  }

  // Poll for marker text to appear (run 13 lesson: fillQuestions fired before
  // the questions step rendered, then send hit the unfilled form). Returns
  // 'ready' or 'timeout'. Call BEFORE fillQuestions / confirmation checks.
  function waitFor(marker, timeoutMs) {
    timeoutMs = timeoutMs || 15000;
    const t0 = Date.now();
    while (Date.now() - t0 < timeoutMs) {
      if (document.body.innerText.indexOf(marker) > -1) return 'ready';
      const s = Date.now(); while (Date.now() - s < 250) {} // 250ms spin
    }
    return 'timeout';
  }

  // Radio-button questions (run 41 lesson: Gateway renders the same
  // questions as radios where Cronos used selects). rules: array of
  // [questionRegex, desiredLabel]. Finds each radio group, walks up to the
  // question text, clicks the label-matching radio.
  function fillRadios(rules) {
    const done = [];
    const radios = Array.from(document.querySelectorAll('input[type=radio]'));
    const groups = {};
    radios.forEach(function (r) { (groups[r.name] = groups[r.name] || []).push(r); });
    Object.keys(groups).forEach(function (n) {
      const gs = groups[n];
      let q = '', g = gs[0];
      for (let d = 0; d < 5 && g; d++) {
        g = g.parentElement;
        const t = norm((g.innerText || g.textContent || ''));
        if (t.length > 20) { q = t; break; }
      }
      let want = null;
      rules.forEach(function (rl) { if (!want && rl[0].test(q)) want = rl[1]; });
      if (want) {
        const hit = gs.find(function (r) {
          const lbl = (r.labels && r.labels[0]) ? r.labels[0].textContent : r.value;
          return norm(lbl) === norm(want);
        });
        if (hit && !hit.checked) { hit.click(); done.push(q.slice(0, 40) + '=' + want); }
      }
    });
    return JSON.stringify(done);
  }

  // Gateway-standard radio answers (run 41). Pass to fillRadios().
  const RADIO_STD = [
    [/legally authorized to work in canada/, 'yes'],
    [/18.*or 19/, 'i am 19+'],
    [/similar position.*contact you/, 'yes'],
    [/current or a past.*employee|past.*employee/, 'no'],
    [/banned from or trespassed/, 'no'],
    [/consent to us conducting searches/, 'yes'],
  ];

  return { uploadResume, fillPersonal, fillQuestions, fillRadios, RADIO_STD, clickNext, send, confirmation, waitFor, STD };
})();
