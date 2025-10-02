# System Report

Generated at: {{ report.metadata.generated_at }}
Total sections: {{ report.metadata.sections }}

## Critical Health Digest

Overall status: `{{ report.health_digest.overall.display_label() }}`

{% if report.health_digest.findings.is_empty() %}
No critical findings detected.
{% else %}
{% for finding in report.health_digest.findings %}
- **{{ finding.severity.display_label() }}** ({{ finding.source_title }}): {{ finding.message }}
{% endfor %}
{% endif %}

{% for section in report.sections %}
## {{ section.title }}

Status: `{{ section.status }}`

{% if let Some(summary) = section.summary %}
> {{ summary }}
{% endif %}

```json
{{ section.body | json | safe }}
```

{% if section.has_notes() %}
**Notes**
{% for note in section.notes %}- {{ note }}
{% endfor %}
{% endif %}

{% endfor %}
