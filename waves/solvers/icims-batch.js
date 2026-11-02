// Shared iCIMS application solver for sin-industry program (DRAFT - live DOM adaptation pending)
// For Cash Money (cash-money-curo.icims.com -> iframe cashmoney-attainfinance.icims.com).
// Browser-side: load via `bridge-cli eval <file>` (visible Chrome) then call ICIMS.* steps.
// Conventions: label-based discovery (jv-batch.js pattern), globalThis idempotent, separate verify evals.

globalThis.ICIMS = (function () {
  // Standard answers (honest, per GOAL.md)
  // PII note: <APPLICANT_*> placeholders must be filled from the operator's
// records before running. Real values are never committed to the repo.
  const STD = {
    firstName: 'Mohammad',
    lastName: 'Movahedi',
    address: '<APPLICANT_ADDRESS>',
    unit: 'Unit 3203',
    email: 'm.h.movahedi97@gmail.com',
    phone: '<APPLICANT_PHONE_DIGITS>', // digits-only
    city: 'Mississauga',
    province: 'Ontario',
    postal: 'L5B 0N3',
    country: 'Canada',
    salary: '120000',
    heard: 'Corporate Website',
  };

  function fire(el) {
    ['input', 'change', 'blur'].forEach(function (t) {
      el.dispatchEvent(new Event(t, { bubbles: true }));
    });
  }

  function norm(s) { return (s || '').toLowerCase().replace(/[^a-z0-9 ]/g, ' ').replace(/\s+/g, ' ').trim(); }

  function labelFor(el) {
    if (el.id) {
      const l = document.querySelector('label[for="' + el.id + '"]');
      if (l) return l.textContent;
    }
    let p = el.parentElement;
    for (let d = 0; d < 5 && p; d++) {
      const l = p.querySelector('label, .icims-field-label, span.icims-label, .field-label');
      if (l && l.textContent.trim().length > 1 && l.textContent.trim().length < 90)
        return l.textContent;
      p = p.parentElement;
    }
    return '';
  }

  // --- Discovery: parse search results into [{id, title, url, location}] ---
  function discoverJobs() {
    const out = [];
    document.querySelectorAll('a').forEach(function (a) {
      const m = (a.getAttribute('href') || '').match(/\/jobs\/(\d+)\/job/);
      if (m) {
        let loc = '';
        const row = a.closest('tr, .row, li, .iCIMS_JobListing');
        if (row) loc = norm(row.textContent).slice(0, 200);
        out.push({ id: m[1], title: a.textContent.trim().slice(0, 120), url: a.href, loc: loc });
      }
    });
    // dedupe by id
    const seen = {}, res = [];
    out.forEach(function (j) { if (!seen[j.id]) { seen[j.id] = 1; res.push(j); } });
    return res;
  }

  // --- Click "Apply for This Job Online" ---
  function clickApply() {
    const cands = Array.from(document.querySelectorAll('a, button, input[type=button], input[type=submit]'));
    for (const el of cands) {
      const t = norm(el.textContent || el.value || '');
      if (/apply for this job online|^apply$/.test(t)) {
        el.scrollIntoView({ block: 'center' });
        el.click();
        return 'CLICKED:' + el.tagName + ':' + (el.textContent || el.value || '').trim().slice(0, 40);
      }
    }
    return 'NO-APPLY-BUTTON';
  }

  // --- Create profile (new user): email + password + name ---
  function createProfile(email, pw) {
    const log = [];
    const set = function (el, v) {
      el.focus();
      el.value = v;
      fire(el);
      log.push(norm(labelFor(el)) + '=' + v.slice(0, 3) + '***');
    };
    document.querySelectorAll('input[type=text], input[type=email], input:not([type])').forEach(function (el) {
      if (el.type === 'file' || el.type === 'hidden' || el.value) return;
      const lab = norm(labelFor(el));
      if (/first.?name/.test(lab)) set(el, STD.firstName);
      else if (/last.?name/.test(lab)) set(el, STD.lastName);
      else if (/e.?mail/.test(lab)) set(el, email);
    });
    document.querySelectorAll('input[type=password]').forEach(function (el, i) {
      if (!el.value) set(el, pw);
    });
    return log;
  }

  // --- Text fields by label ---
  function fillText(rules) {
    // rules: [[/regex/, value], ...]
    const done = [];
    document.querySelectorAll('input[type=text], input[type=email], input[type=tel], input:not([type]), textarea').forEach(function (el) {
      if (el.type === 'file' || el.type === 'hidden' || el.value) return;
      const lab = norm(labelFor(el));
      for (const [re, v] of rules) {
        if (re.test(lab)) { el.focus(); el.value = v; fire(el); done.push(lab + '=' + v); break; }
      }
    });
    return done;
  }

  function personalRules() {
    return [
      [/first.?name/, STD.firstName],
      [/last.?name/, STD.lastName],
      [/e.?mail/, STD.email],
      [/primary.?phone|^phone|mobile|telephone|cell/, STD.phone],
      [/address.*line.?1|street.?address/, STD.address + ', ' + STD.unit],
      [/address/, STD.address + ', ' + STD.unit],
      [/city/, STD.city],
      [/postal|zip/, STD.postal],
    ];
  }

  // --- Selects by label ---
  function fillSelects(rules) {
    const done = [];
    document.querySelectorAll('select').forEach(function (el) {
      const lab = norm(labelFor(el));
      for (const [re, want] of rules) {
        if (!re.test(lab)) continue;
        const opts = Array.from(el.options);
        const hit = opts.find(o => norm(o.text).includes(norm(want))) ||
                    opts.find(o => norm(o.text).startsWith(norm(want).slice(0, 4)));
        if (hit) { el.value = hit.value; fire(el); done.push(lab + '->' + hit.text.trim()); }
        else done.push(lab + '->OPTION-NOT-FOUND(want=' + want + ')');
        break;
      }
    });
    return done;
  }

  function geoRules() {
    return [
      [/province|state/, STD.province],
      [/country/, STD.country],
    ];
  }

  // --- Radios: match question text -> nearest radio group, click label-matched option ---
  function fillRadios(rules) {
    // rules: [[/question-regex/, /answer-regex/], ...]
    const done = [];
    for (const [qre, are] of rules) {
      let matched = false;
      document.querySelectorAll('fieldset, .form-group, .icims-question, tr').forEach(function (box) {
        if (matched) return;
        const q = norm(box.textContent);
        if (!qre.test(q)) return;
        const radios = Array.from(box.querySelectorAll('input[type=radio]'));
        if (!radios.length) return;
        for (const r of radios) {
          let rl = '';
          if (r.id) { const l = document.querySelector('label[for="' + r.id + '"]'); if (l) rl = norm(l.textContent); }
          if (!rl) rl = norm(r.parentElement ? r.parentElement.textContent : '');
          if (are.test(rl)) {
            r.scrollIntoView({ block: 'center' });
            r.click();
            done.push(qre.source + '->' + rl.slice(0, 40) + (r.checked ? '(checked)' : '(NOT-CHECKED)'));
            matched = true;
            break;
          }
        }
      });
      if (!matched) done.push(qre.source + '->NO-MATCH');
    }
    return done;
  }

  // --- Checkboxes: check consent/agreement boxes ---
  function fillCheckboxes(patterns) {
    const done = [];
    document.querySelectorAll('input[type=checkbox]').forEach(function (cb) {
      let lab = '';
      if (cb.id) { const l = document.querySelector('label[for="' + cb.id + '"]'); if (l) lab = norm(l.textContent); }
      if (!lab) lab = norm(cb.parentElement ? cb.parentElement.textContent : '');
      for (const re of patterns) {
        if (re.test(lab) && !cb.checked) { cb.scrollIntoView({ block: 'center' }); cb.click(); done.push(lab.slice(0, 60) + (cb.checked ? '(checked)' : '(NOT-CHECKED)')); }
      }
    });
    return done;
  }

  // --- Resume upload via DataTransfer ---
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

  // --- Submit: find the primary submit button ---
  function submit() {
    const cands = Array.from(document.querySelectorAll('button, input[type=submit], input[type=button], a'));
    for (const el of cands) {
      const t = norm(el.textContent || el.value || '');
      if (/^(submit application|submit|save and continue|continue|next)$/.test(t)) {
        el.scrollIntoView({ block: 'center' });
        el.click();
        return 'CLICKED:' + el.tagName + ':' + t.slice(0, 40);
      }
    }
    return 'NO-SUBMIT-BUTTON';
  }

  // --- Confirmation detection ---
  function confirmation() {
    const body = norm(document.body ? document.body.textContent : '');
    const url = location.href;
    const sent = /application has been (submitted|received)|thank you for (your|applying)|your application was (submitted|received)|successfully submitted/.test(body);
    return { sent: sent, url: url, title: document.title };
  }

  // --- Validation errors snapshot ---
  function errors() {
    const errs = [];
    document.querySelectorAll('.error, .icims-error, [aria-invalid="true"], .field-validation-error, .validation-summary-errors').forEach(function (el) {
      const t = (el.textContent || '').trim().slice(0, 120);
      if (t) errs.push(t);
    });
    return errs.slice(0, 20);
  }

  function state() {
    return { url: location.href, title: document.title, ready: document.readyState };
  }

  return {
    STD: STD, discoverJobs: discoverJobs, clickApply: clickApply, createProfile: createProfile,
    fillText: fillText, personalRules: personalRules, fillSelects: fillSelects, geoRules: geoRules,
    fillRadios: fillRadios, fillCheckboxes: fillCheckboxes, uploadResume: uploadResume,
    submit: submit, confirmation: confirmation, errors: errors, state: state,
  };
})();
'ICIMS-LOADED';
