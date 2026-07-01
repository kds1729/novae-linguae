"""Certification records: `subject` (the fn_… attested to, indexed) + `certified` (the verdict)."""

from django.db import migrations, models


class Migration(migrations.Migration):

    dependencies = [
        ('commons', '0004_typed_query_gin'),
    ]

    operations = [
        migrations.AddField(
            model_name='record',
            name='subject',
            field=models.CharField(blank=True, db_index=True, max_length=128, null=True),
        ),
        migrations.AddField(
            model_name='record',
            name='certified',
            field=models.BooleanField(blank=True, null=True),
        ),
    ]
