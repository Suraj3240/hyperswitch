use api_models::analytics::{
    payments::{PaymentDimensions, PaymentFilters, PaymentMetrics, PaymentMetricsBucketIdentifier},
    Granularity, TimeRange,
};
use common_enums::enums as storage_enums;
use time::PrimitiveDateTime;

use crate::analytics::{
    query::{Aggregate, GroupByClause, ToSql},
    types::{AnalyticsCollection, AnalyticsDataSource, DBEnumWrapper, LoadRow, MetricsResult},
};

mod avg_ticket_size;
mod payment_count;
mod payment_processed_amount;
mod payment_success_count;
mod success_rate;

use avg_ticket_size::AvgTicketSize;
use payment_count::PaymentCount;
use payment_processed_amount::PaymentProcessedAmount;
use payment_success_count::PaymentSuccessCount;
use success_rate::PaymentSuccessRate;

#[derive(Debug, PartialEq, Eq)]
pub struct PaymentMetricRow {
    pub currency: Option<DBEnumWrapper<storage_enums::Currency>>,
    pub status: Option<DBEnumWrapper<storage_enums::AttemptStatus>>,
    pub connector: Option<String>,
    pub authentication_type: Option<DBEnumWrapper<storage_enums::AuthenticationType>>,
    pub payment_method: Option<String>,
    pub total: Option<bigdecimal::BigDecimal>,
    pub count: Option<i64>,
    pub start_bucket: Option<PrimitiveDateTime>,
    pub end_bucket: Option<PrimitiveDateTime>,
}

pub trait PaymentMetricAnalytics: LoadRow<PaymentMetricRow> {}

#[async_trait::async_trait]
pub trait PaymentMetric<T>
where
    T: AnalyticsDataSource + PaymentMetricAnalytics,
{
    async fn load_metrics(
        &self,
        dimensions: &[PaymentDimensions],
        merchant_id: &str,
        filters: &PaymentFilters,
        granularity: &Option<Granularity>,
        time_range: &TimeRange,
        pool: &T,
    ) -> MetricsResult<Vec<(PaymentMetricsBucketIdentifier, PaymentMetricRow)>>;
}

#[async_trait::async_trait]
impl<T> PaymentMetric<T> for PaymentMetrics
where
    T: AnalyticsDataSource + PaymentMetricAnalytics,
    PrimitiveDateTime: ToSql<T>,
    AnalyticsCollection: ToSql<T>,
    Granularity: GroupByClause<T>,
    Aggregate<&'static str>: ToSql<T>,
{
    async fn load_metrics(
        &self,
        dimensions: &[PaymentDimensions],
        merchant_id: &str,
        filters: &PaymentFilters,
        granularity: &Option<Granularity>,
        time_range: &TimeRange,
        pool: &T,
    ) -> MetricsResult<Vec<(PaymentMetricsBucketIdentifier, PaymentMetricRow)>> {
        match self {
            Self::PaymentSuccessRate => {
                PaymentSuccessRate
                    .load_metrics(
                        dimensions,
                        merchant_id,
                        filters,
                        granularity,
                        time_range,
                        pool,
                    )
                    .await
            }
            Self::PaymentCount => {
                PaymentCount
                    .load_metrics(
                        dimensions,
                        merchant_id,
                        filters,
                        granularity,
                        time_range,
                        pool,
                    )
                    .await
            }
            Self::PaymentSuccessCount => {
                PaymentSuccessCount
                    .load_metrics(
                        dimensions,
                        merchant_id,
                        filters,
                        granularity,
                        time_range,
                        pool,
                    )
                    .await
            }
            Self::PaymentProcessedAmount => {
                PaymentProcessedAmount
                    .load_metrics(
                        dimensions,
                        merchant_id,
                        filters,
                        granularity,
                        time_range,
                        pool,
                    )
                    .await
            }
            Self::AvgTicketSize => {
                AvgTicketSize
                    .load_metrics(
                        dimensions,
                        merchant_id,
                        filters,
                        granularity,
                        time_range,
                        pool,
                    )
                    .await
            }
        }
    }
}
