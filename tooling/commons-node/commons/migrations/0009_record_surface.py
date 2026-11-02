from django.db import migrations, models


class Migration(migrations.Migration):

    dependencies = [
        ('commons', '0008_witness'),
    ]

    operations = [
        migrations.AddField(
            model_name='record',
            name='surface',
            field=models.JSONField(default=dict),
        ),
    ]
