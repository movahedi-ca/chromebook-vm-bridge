// Shared UKG Pro (Ultipro) application solver for sin-industry program (DRAFT - live DOM adaptation pending)
// For Mark Anthony Group (recruiting2.ultipro.com/MAR5000MAG/JobBoard/<boardId>/).
// Browser-side: load via `bridge-cli --lease $L eval "$(cat ukg-batch.js)"` (visible Chrome) then call UKG.* steps.
// Conventions: label-based discovery (icims-batch.js pattern), globalThis idempotent, separate verify evals.

globalThis.UKG = (function () {
  // Standard answers (honest, per GOAL.md)
  const STD = {
    firstName: 'Mohammad',
    lastName: 'Movahedi',
    address: '36 Elm Dr W',
    unit: 'Unit 3203',
    email: 'm.h.movahedi97@gmail.com',
    phone: '4372562256', // digits-only
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
    // UKG ignite web-components: label may be a sibling or in shadow DOM; walk up for text
    let p = el.parentElement;
    for (let d = 0; d < 6 && p; d++) {
      const l = p.querySelector('label, .form-label, span.label, igc-label, [data-label]');
      if (l && l.textContent.trim().length > 1 && l.textContent.trim().length < 90)
        return l.textContent;
      // also try preceding siblings
      let s = el.previousElementSibling;
      for (let k = 0; k < 3 && s; k++) {
        if (/label/i.test(s.tagName) && s.textContent.trim().length > 1) return s.textContent;
        s = s.previousElementSibling;
      }
      p = p.parentElement;
    }
    return '';
  }

  // --- Discovery: parse rendered opportunity list into [{req, title, url}] ---
  function discoverJobs() {
    const out = [];
    document.querySelectorAll('a').forEach(function (a) {
      const href = a.getAttribute('href') || '';
      if (/OpportunityDetail/i.test(href)) {
        const row = a.closest('tr, .row, li, [role="row"], .opportunity');
        const txt = norm(row ? row.textContent : a.textContent);
        const reqm = txt.match(/([A-Z]{3,}[0-9]{4,})/) || href.match(/([A-Z]{3,}[0-9]{4,})/);
        out.push({ req: reqm ? reqm[1] : '', title: a.textContent.trim().slice(0, 120), url: a.href, ctx: txt.slice(0, 160) });
      }
    });
    const seen = {}, res = [];
    out.forEach(function (j) { const k = j.req || j.title; if (!seen[k]) { seen[k] = 1; res.push(j); } });
    return res;
  }

  // --- Click Apply on the OpportunityDetail page ---
  function clickApply() {
    const cands = Array.from(document.querySelectorAll('a, button, input[type=button], input[type=submit]'));
    for (const el of cands) {
      const t = norm(el.textContent || el.value || '');
      if (/^apply( now)?$|apply for this/.test(t)) {
        el.scrollIntoView({ block: 'center' });
        el.click();
        return 'CLICKED:' + el.tagName + ':' + t.slice(0, 40);
      }
    }
    return 'NO-APPLY-BUTTON';
  }

  // --- Text fields by label ---
  function fillText(rules) {
    const done = [];
    document.querySelectorAll('input[type=text], input[type=email], input[type=tel], input:not([type]), textarea').forEach(function (el) {
      if (el.type === 'file' || el.type === 'hidden' || el.value) return;
      const lab = norm(labelFor(el));
      for (const [re, v] of rules) {
        if (re.test(lab)) { el.focus(); el.value = v; fire(el); done.push(lab + '=' + String(v).slice(0, 20)); break; }
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
      [/address.*line.?1|street/, STD.address + ', ' + STD.unit],
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

  // --- Radios ---
  function fillRadios(rules) {
    const done = [];
    for (const [qre, are] of rules) {
      let matched = false;
      document.querySelectorAll('fieldset, .form-group, [role="radiogroup"], tr, .question').forEach(function (box) {
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

  // --- Checkboxes ---
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

  // --- Submit ---
  function submit() {
    const cands = Array.from(document.querySelectorAll('button, input[type=submit], input[type=button], a'));
    for (const el of cands) {
      const t = norm(el.textContent || el.value || '');
      if (/^(submit application|submit|save and continue|continue|next|finish)$/.test(t)) {
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
    const sent = /application has been (submitted|received)|thank you for (your|applying)|your application was (submitted|received)|successfully submitted|application complete/.test(body);
    return { sent: sent, url: url, title: document.title };
  }

  // --- Validation errors snapshot ---
  function errors() {
    const errs = [];
    document.querySelectorAll('.error, [aria-invalid="true"], .field-validation-error, .validation-summary-errors, .text-danger').forEach(function (el) {
      const t = (el.textContent || '').trim().slice(0, 120);
      if (t) errs.push(t);
    });
    return errs.slice(0, 20);
  }

  function state() {
    return { url: location.href, title: document.title, ready: document.readyState };
  }

  return {
    STD: STD, discoverJobs: discoverJobs, clickApply: clickApply,
    fillText: fillText, personalRules: personalRules, fillSelects: fillSelects, geoRules: geoRules,
    fillRadios: fillRadios, fillCheckboxes: fillCheckboxes,
    submit: submit, confirmation: confirmation, errors: errors, state: state,
  };
})();
'UKG-LOADED';
