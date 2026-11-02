(function(){
  var btn = null;
  document.querySelectorAll('button').forEach(function(b){
    if (/Proceed to Review/i.test(b.textContent.trim()) && b.getBoundingClientRect().y > 0 && !btn) btn = b;
  });
  if(!btn) return 'NOBTN';
  btn.scrollIntoView({block:'center'});
  btn.click();
  return 'clicked';
})()
