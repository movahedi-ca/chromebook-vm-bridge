// Shared Workday application solver for sin-industry program
// Generalized from Canopy Growth runs 01-05 (Nov 1, 2026)
// Timing: 109-146s per application (beating OLG 147s final)

// Standard answers (honest, per GOAL.md)
// PII note: <APPLICANT_*> placeholders must be filled from the operator's
// records before running. Real values are never committed to the repo.
  const STD = {
  lastName: 'Movahedi',
  address: '<APPLICANT_ADDRESS>',
  postal: 'L5B 0N3',
  phone: '<APPLICANT_PHONE_DIGITS>', // digits-only, Workday rejects formatted
  howHeard: 'Corporate Website',
  eligible: 'Yes',
  family: 'No',
  country: 'Canada',
  age19: 'Yes',
  gender: 'Male',
  racial: 'Middle Eastern or West Asian', // matches "Middle Eastern or West Asian(e.g. Lebanon, Iran..."
};

// Step 2: My Information - fill text fields via direct value + React events (faster than bridge fill)
function fillMyInformation() {
  const fields = {
    'name--legalName--lastName': STD.lastName,
    'address--addressLine1': STD.address,
    'address--postalCode': STD.postal,
    'phoneNumber--phoneNumber': STD.phone,
  };
  for (const [id, val] of Object.entries(fields)) {
    const el = document.querySelector(`[id="${id}"]`);
    if (el) {
      el.value = val;
      el.dispatchEvent(new Event('input', {bubbles: true}));
      el.dispatchEvent(new Event('change', {bubbles: true}));
    }
  }
}

// Helper: select dropdown option by text
function selectDropdown(button, optionText, callback) {
  button.click();
  setTimeout(() => {
    const opt = Array.from(document.querySelectorAll('[role="option"]'))
      .find(o => o.textContent.trim() === optionText || o.textContent.includes(optionText));
    if (opt) opt.click();
    setTimeout(callback, 800);
  }, 1300);
}

// Step 2 continued: How Heard + Previously Worked
function fillMyInformationDropdowns(callback) {
  const heard = Array.from(document.querySelectorAll('button')).find(b =>
    /how did you hear/i.test(b.getAttribute('aria-label') || '') && b.textContent.trim() === 'Select One');
  if (heard) {
    selectDropdown(heard, STD.howHeard, () => {
      const noRadio = Array.from(document.querySelectorAll('input[type="radio"]')).find(r => {
        const l = document.querySelector(`label[for="${r.id}"]`);
        return l && l.textContent.trim() === 'No';
      });
      if (noRadio && !noRadio.checked) noRadio.click();
      setTimeout(callback, 800);
    });
  } else callback();
}

// Step 4: Application Questions (Yes/No/Canada + conditional Age)
function fillApplicationQuestions(callback) {
  const answers = [STD.eligible, STD.family, STD.country];
  const btns = Array.from(document.querySelectorAll('button')).filter(b => b.textContent.trim() === 'Select One');
  let i = 0;
  function next() {
    if (i >= Math.min(btns.length, answers.length)) {
      // Check for conditional age question
      setTimeout(() => {
        const ageBtn = Array.from(document.querySelectorAll('button')).find(b => b.textContent.trim() === 'Select One');
        if (ageBtn) {
          selectDropdown(ageBtn, STD.age19, callback);
        } else callback();
      }, 1000);
      return;
    }
    selectDropdown(btns[i], answers[i], () => { i++; setTimeout(next, 500); });
  }
  next();
}

// Step 5: Voluntary Disclosures (Gender, Racial, Terms)
function fillVoluntaryDisclosures(callback) {
  const gBtn = Array.from(document.querySelectorAll('button')).find(b =>
    /gender/i.test(b.getAttribute('aria-label') || '') && b.textContent.trim() === 'Select One');
  if (!gBtn) { callback(); return; }
  selectDropdown(gBtn, STD.gender, () => {
    const rBtn = Array.from(document.querySelectorAll('button')).find(b =>
      /racial|ethnic/i.test(b.getAttribute('aria-label') || '') && b.textContent.trim() === 'Select One');
    if (rBtn) {
      selectDropdown(rBtn, STD.racial, () => {
        const cb = Array.from(document.querySelectorAll('input[type="checkbox"]')).find(c => {
          const l = document.querySelector(`label[for="${c.id}"]`);
          return l && /terms and conditions/i.test(l.textContent);
        });
        if (cb && !cb.checked) cb.click();
        setTimeout(callback, 800);
      });
    } else callback();
  });
}

// Click Save and Continue
function clickSaveAndContinue() {
  const b = Array.from(document.querySelectorAll('button')).find(x => x.textContent.trim() === 'Save and Continue');
  if (b) b.click();
}

// Click Submit
function clickSubmit() {
  const b = Array.from(document.querySelectorAll('button')).find(x => x.textContent.trim() === 'Submit');
  if (b) b.click();
}

// GENERALIZED FIX (2026-11-02, Labatt run 16): Workday virtualized multiselect
// (react-virtualized, 200+ options). DOM clicks land on recycled nodes; wd-multi.js
// alone may not persist. The reliable combo to RENDER the selection pill:
//   1. Find the list component via __reactFiber scan (state.items.length > 50)
//   2. const idx = items.findIndex(it => /^target$/i.test((it.label||"").trim()))
//   3. sn.updateItemMultiSelect(idx)          // sets internal list state
//   4. item.onSelect([item], new MouseEvent("click",{bubbles:true,cancelable:true}), item)  // notifies parent
// NOTE: on Labatt's CXS build the pill renders but does NOT persist to the submit
// model (Review shows "No Response"). Standard wd3 tenants unaffected. See run-16.md.

// GENERALIZED (2026-11-02, Labatt run 16): CXS deep-link routing.
// On wd1.myworkdaysite.com, direct navigation to /job/.../apply 404s or drops to
// search. Sequential nav works: navigate to /job/<loc>/<title>_<reqid> first,
// THEN navigate to the /apply URL. The Apply anchor's href (verified in-page)
// gives the exact /apply path; never guess it.
