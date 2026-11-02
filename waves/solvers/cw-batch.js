// Shared Oracle Recruiting Cloud (ORC) application solver for sin-industry program
// Generalized from Caesars Windsor runs 51-55 (Nov 1, 2026).
// VISIBLE CHROME ONLY per 2026-11-01 display rule: load via `bridge-cli eval "<file>"`
// (default visible context, active tab). Never use pool contexts for form work.
// Portal: https://edmn.fa.us2.oraclecloud.com/hcmUI/CandidateExperience/en/sites/CX_1
// DNS: Control D blocks www.caesars.com (0.0.0.0); /etc/hosts on VM maps
//   www.caesars.com + caesars.com + caesarswindsor.com -> 45.60.31.125 (Incapsula edge,
//   confirmed via DoH CNAME dpkjv.x.incapdns.net). Do NOT use 198.18.230.216
//   (unreachable from the VM).

globalThis.CW = (function () {
  // Standard answers (honest, per GOAL.md)
  const STD = {
    firstName: 'Mohammad',
    lastName: 'Movahedi',
    email: 'm.h.movahedi97@gmail.com',
    phone: '4372562256',
    phoneIntl: '+14372562256',
    address: '36 Elm Dr W, Unit 3203',
    city: 'Mississauga',
    province: 'Ontario',
    postal: 'L5B 0N3',
    country: 'Canada',
    heard: 'Corporate Website',
  };

  // JET/React-safe text fill: focus + execCommand insertText + blur.
  // Direct .value sets are ignored by Oracle JET form components.
  function fillText(el, text) {
    el.focus();
    el.select && el.select();
    document.execCommand('insertText', false, text);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    el.blur();
    return el.value;
  }

  function findInput(labelRe) {
    const re = new RegExp(labelRe, 'i');
    return Array.from(document.querySelectorAll('input')).find(function (i) {
      const lab = (i.placeholder || '') + ' ' + (i.getAttribute('aria-label') || '') +
        ' ' + (i.name || '') + ' ' + (i.id || '');
      return re.test(lab);
    });
  }

  function findButton(textRe) {
    const re = new RegExp(textRe, 'i');
    return Array.from(document.querySelectorAll('button')).find(function (b) {
      return re.test(b.textContent.trim().replace(/\s+/g, ' '));
    });
  }

  function click(el) {
    if (!el) return false;
    el.scrollIntoView({ block: 'center' });
    el.click();
    return true;
  }

  // --- Step 0: search Windsor jobs from the CX_1 jobs page ---
  function searchWindsor() {
    const loc = findInput('City, state, country');
    if (loc) fillText(loc, 'Windsor, ON, Canada');
    const find = findButton('^FIND$');
    return { loc: !!loc, findBtn: !!find };
  }

  // --- Step 1: list job cards (title + location + url) ---
  function listJobs() {
    return Array.from(document.querySelectorAll('a'))
      .filter(function (a) { return /\/jobs\//.test(a.href); })
      .map(function (a) {
        return a.href + ' :: ' + a.textContent.trim().replace(/\s+/g, ' ').slice(0, 90);
      })
      .filter(function (s, i, arr) { return arr.indexOf(s) === i; })
      .slice(0, 40);
  }

  return { STD: STD, fillText: fillText, findInput: findInput, findButton: findButton,
           click: click, searchWindsor: searchWindsor, listJobs: listJobs };
})();
