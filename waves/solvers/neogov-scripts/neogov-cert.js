(function(){
  var btn = null;
  document.querySelectorAll('button').forEach(function(b){
    if (/Certify|Submit/i.test(b.textContent.trim()) && b.getBoundingClientRect().y > 0 && !btn) btn = b;
  });
  if(!btn) return 'NOBTN';
  btn.scrollIntoView({block:'center'});
  btn.click();
  return 'clicked:'+btn.textContent.trim().slice(0,40);
})()
