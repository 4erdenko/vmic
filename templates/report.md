# Системный отчёт

Сгенерирован: {{ report.metadata.generated_at }}

{% for section in report.sections %}
## {{ section.title }}

Статус: `{{ section.status }}`

{% if let Some(summary) = section.summary %}
> {{ summary }}
{% endif %}

```json
{{ section.body | json | safe }}
```

{% if section.has_notes() %}
**Примечания**
{% for note in section.notes %}- {{ note }}
{% endfor %}
{% endif %}

{% endfor %}
