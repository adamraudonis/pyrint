from alembic import op
import sqlalchemy as sa


def upgrade():
    op.create_table(
        'dataset_comparison_report',
        sa.Column('is_published', sa.Boolean(), nullable=True)
    )
