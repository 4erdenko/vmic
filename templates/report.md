# System Report

Generated at: {{ report.metadata.generated_at }}
Total sections: {{ report.metadata.sections }}

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
