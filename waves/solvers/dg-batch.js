// Diageo Workday solver patterns (dg-batch.js)
// Developed during Diageo runs 36-40 (2026-11-02). Tenant: diageo.wd3.myworkdayjobs.com
// NOTE: applications on this tenant are BLOCKED by the How-Heard virtualized combobox
// (see run-36.md). These patterns are the reusable parts.

// --- Pattern 1: React controlled-input fill that actually updates state ---
// On this Workday build, bridge fill and el.value= do NOT update React state.
// Must invoke the props handler directly after the native setter.
function dgSetInput(id, v) {
  var el = document.getElementById(id);
  if (!el) return id + ':gone';
  var pk = Object.keys(el).find(function (k) { return k.indexOf('__reactProps') === 0; });
  var props = el[pk];
  var set = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
  set.call(el, v);
  var h = props.onChange || props.onInput; // this tenant uses onInput on some fields
  if (typeof h === 'function') { try { h({ target: el, currentTarget: el }); } catch (e) {} }
  return id + ':ok';
}
// Verified fields: name--legalName--lastName, address--addressLine1, address--city,
// address--postalCode, phoneNumber--phoneNumber (digits-only), source--source (text only)

// --- Pattern 2: Resume upload via DataTransfer (headless pool) ---
// Returns 'files:0' immediately but the change event triggers async React upload;
// poll body text for 'Successfully Uploaded!' afterwards.
function dgUploadResume(b64, filename) {
  filename = filename || 'Movahedi-Resume-Standard.pdf';
  var bytes = atob(b64), arr = new Uint8Array(bytes.length);
  for (var i = 0; i < bytes.length; i++) arr[i] = bytes.charCodeAt(i);
  var file = new File([new Blob([arr], { type: 'application/pdf' })], filename, { type: 'application/pdf' });
  var input = document.querySelector('input[type=file]');
  if (!input) return 'NO-INPUT';
  var dt = new DataTransfer();
  dt.items.add(file);
  input.files = dt.files;
  input.dispatchEvent(new Event('change', { bubbles: true }));
  return 'sent';
}

// --- Pattern 3: Fiber scan for Canvas Kit virtualized list ---
// Finds the list component whose memoizedState.items matches a label predicate.
function dgFindList(labelRe, minItems) {
  minItems = minItems || 3;
  var all = document.querySelectorAll('div');
  for (var i = 0; i < all.length; i++) {
    var el = all[i];
    var fk = Object.keys(el).find(function (k) { return k.indexOf('__reactFiber') === 0; });
    if (!fk) continue;
    var n = el[fk];
    for (var d = 0; d < 15 && n; d++) {
      var st = n.memoizedState;
      if (st && st.items && Array.isArray(st.items) && st.items.length >= minItems) {
        var labels = st.items.map(function (x) { return (x.label || '').trim(); });
        if (labels.some(function (l) { return labelRe.test(l); })) return { fiber: n, items: st.items };
      }
      n = n.return;
    }
  }
  return null;
}
// List stateNode exposes: updateItemMultiSelect, updateItemSingleSelect, onSelectWrapper,
// handleSelectionEvent, changeSelectionStateForListItems. Item has onSelect.
// WARNING: item.onSelect is `e=>{"length"in e?ee(e):Q(e)}`; calling with a synthetic
// event lacking a target silently no-ops; calling with ([item], ev, item) CRASHES React.
// updateItemMultiSelect alone updates list state but NOT the parent form model.

// --- Pattern 4: Canvas Kit combobox exact-text quirk ---
// Setting the input text to the EXACT option label via dgSetInput clears BLUR
// validation (aria-invalid=false) but SUBMIT validation still requires a real item
// selection. Do not trust aria-invalid alone; always verify via submit attempt.

// --- Diageo jobs API (for role discovery; www.diageo.com is DNS-blocked on the VM) ---
// https://diageo-prod-api.connectid.cloud/api/jobs?page=N  -> {data, facets, meta}
// Filter: Country == 'Canada'. Fields: referenceID, Job_Posting_Title,
// Primary_Job_Posting_Location, Job_Family, External_Posting_URL (full apply URL),
// Job_Requisition_Status, Job_Posting_Start_Date.
