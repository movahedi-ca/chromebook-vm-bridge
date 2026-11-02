(function(){
  var log = [];
  function set(id, val){
    var el = document.getElementById(id);
    if(!el) { log.push(id+':missing'); return; }
    el.scrollIntoView({block:'center'});
    el.focus(); el.value = val;
    el.dispatchEvent(new Event('input',{bubbles:true}));
    el.dispatchEvent(new Event('change',{bubbles:true}));
    el.blur();
    log.push(id+'='+el.value);
  }
  set('today-date-input','2026-11-02');
  set('employee-id-input','NA');
  set('initials-text','MM');
  return JSON.stringify(log);
})()
