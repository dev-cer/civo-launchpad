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

# The workshop register page (/register) is rendered by the backend (Askama + HTMX): backend/templates/register/.
