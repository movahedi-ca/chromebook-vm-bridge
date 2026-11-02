(function(){
  var log = [];
  function clickRadio(prefix, val){
    var n = 0;
    document.querySelectorAll('input[type=radio]').forEach(function(r){
      if (r.name.indexOf(prefix) === 0 && r.value === val && r.getBoundingClientRect().y > 0) { r.click(); n++; }
    });
    log.push(prefix.slice(0,25)+'='+val+':'+n);
  }
  clickRadio('qa-awqs-0confirm', '1');
  clickRadio('qa-awqs-1confirm', '1');
  clickRadio('qa-awqs-2are-you-here', '1');
  clickRadio('qa-awqs-3have-you-previously', '0');
  clickRadio('qa-awqs-4do-you-have-a-valid', '0');
  clickRadio('qa-awqs-5are-you-a-current', '0');
  clickRadio('qa-awqs-13employment', '583915');
  clickRadio('qa-awqs-14what-is-your-gender', '580972');
  clickRadio('qa-awqs-15do-you-identify-as-an-indigenous', '0');
  clickRadio('qa-awqs-16are-you-a-member-of-a-racialized', '1');
  clickRadio('qa-awqs-17are-you-a-person-with-a-disability', '0');
  clickRadio('qa-awqs-18immediate', '583979');
  clickRadio('qa-awqs-20where-did-you-first', '583914');
  clickRadio('qa-awqs-21i-have-read', '1');
  document.querySelectorAll('textarea').forEach(function(el){
    var blob = (el.id||'')+'|'+(el.name||'');
    if (/members-of/.test(blob)) {
      el.focus(); el.value = 'NA';
      el.dispatchEvent(new Event('input',{bubbles:true}));
      el.dispatchEvent(new Event('change',{bubbles:true}));
      el.blur(); log.push('Q20');
    }
  });
  document.querySelectorAll('select').forEach(function(s){
    var id = s.id, lbl = '';
    if (id) { var l = document.querySelector('label[for="'+id+'"]'); if (l) lbl = l.textContent; }
    if (/Which union/.test(lbl)) { s.value='583925'; s.dispatchEvent(new Event('change',{bubbles:true})); log.push('union'); }
    if (/Languages Spoken/.test(lbl)) {
      for (var i=0;i<s.options.length;i++) if (s.options[i].value==='583929') s.options[i].selected = true;
      s.dispatchEvent(new Event('change',{bubbles:true})); log.push('lang');
    }
  });
  return JSON.stringify(log);
})()
