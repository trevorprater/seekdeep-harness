import re

# Recurring ASR misreadings of proper nouns and jargon in this talk. Only
# substitutions whose intended term is unambiguous from the slides or context.
RULES = [
    (r'\b(?:Grokbot|Grogbot|GrogBot|Grochbot|Grochbat|Grockbot|Groundbod|GroundBod|Gropot|Rockbot|rockbot|rock bot|Grog[Bb]ot|Grok ?[Bb]ot)\b', 'Grok Bot'),
    (r"\b(?:full|poll) requests?\b", 'pull requests'),
    (r"\bslop-pill requests\b", 'slop pull requests'),
    (r'\bPotato Annex\b', 'poteto'),
    (r'\bPotato with an E\b', 'poteto with an e'),
    (r'\bP-?[Ss]nack\b', 'pstack'),
    (r'\bP-?[Ss]tack\b', 'pstack'),
    (r'\bbug (?:bot|bar)\b', 'Bugbot'),
    (r'\bBug (?:Bot|Bar)\b', 'Bugbot'),
    (r'\blit rules\b', 'lint rules'),
    (r'\bheat snapshots\b', 'heap snapshots'),
    (r'\btech (?:depth|-?dap)\b', 'tech debt'),
    (r'\bTLA plus\b', 'TLA+'),
    (r'\bJune application\b', 'Dune application'),
    (r'\bDunn\b', 'Dune'),
    (r'\bCentury [Aa]lerts\b', 'Sentry alerts'),
    (r'\bcoach should live\b', 'code should live'),
    (r'\btranscript parts\b', 'transcript cards'),
    (r"\bhe'll climb\b", 'hill climb'),
    (r'\bdeep out the application\b', 'debug the application'),
    (r'\bpick traces\b', 'take traces'),
    (r'\bcorrecting and interviewing\b', 'correcting and intervening'),
    (r'\bthat expiration\b', 'that exploration'),
    (r'\bPlanet Scale\b', 'PlanetScale'),
    (r'\bChrome Dev Tools\b', 'Chrome DevTools'),
    (r'\bDevTool Protocol\b', 'DevTools Protocol'),
]
COMPILED = [(re.compile(p), r) for p, r in RULES]

def fix(text):
    for pat, rep in COMPILED:
        text = pat.sub(rep, text)
    return text
