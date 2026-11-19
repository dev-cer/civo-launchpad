import re, sys, json, pathlib
src = pathlib.Path('frontend/static/index.html').read_text()
links = json.loads(sys.argv[1]) if len(sys.argv)>1 else {}
TITLES = {'webinar':'Kubernetes at Scale · Webinar Journey · Civo Launchpad',
          'workshop':'Beyond the Container · Workshop 26 Nov · Civo Launchpad',
          'academy':'Civo Kubernetes Academy · Launchpad'}
out = pathlib.Path('dist'); out.mkdir(exist_ok=True)
for ed,t in TITLES.items():
    s = src.replace("const EDITION = 'academy';", f"const EDITION = '{ed}';", 1)
    s = re.sub(r"<title>.*?</title>", f"<title>{t}</title>", s, count=1)
    for k,v in links.items():
        s = s.replace(f"{k}:''", f"{k}:'{v}'", 1)
    (out/ed).mkdir(exist_ok=True); (out/ed/'index.html').write_text(s)
    print(ed, len(s))

# Workshop-only landing + register page, served at /register/ (separate from the Launchpad app).
logo = pathlib.Path('backend/src/civo-logo.svg').read_text().strip()
logo = re.sub(r'<\?xml[^>]*\?>', '', logo).strip()
reg = pathlib.Path('frontend/static/register.html').read_text()
reg = reg.replace('__LOGO__', logo.replace('<svg ', '<svg role="img" aria-label="Civo" ', 1))
reg = reg.replace('__LOGO_LG__', logo.replace('<svg ', '<svg role="img" aria-label="Civo" style="height:40px;width:auto" ', 1))
(out/'workshop'/'register').mkdir(parents=True, exist_ok=True)
(out/'workshop'/'register'/'index.html').write_text(reg)
print('workshop/register', len(reg))
