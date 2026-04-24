from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [
        ("posthog", "1121_add_last_realtime_cohort_calculation_at_to_cohort"),
    ]

    operations = [
        migrations.AddField(
            model_name="filesystemshortcut",
            name="order",
            field=models.IntegerField(default=0),
        ),
    ]
