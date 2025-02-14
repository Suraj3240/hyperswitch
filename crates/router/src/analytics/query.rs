#![allow(dead_code)]
use std::marker::PhantomData;

use api_models::{
    analytics::{
        self as analytics_api,
        payments::PaymentDimensions,
        refunds::{RefundDimensions, RefundType},
        Granularity,
    },
    enums::Connector,
    refunds::RefundStatus,
};
use common_enums::{
    enums as storage_enums,
    enums::{AttemptStatus, AuthenticationType, Currency, PaymentMethod},
};
use common_utils::errors::{CustomResult, ParsingError};
use error_stack::{IntoReport, ResultExt};
use router_env::logger;

use super::types::{AnalyticsCollection, AnalyticsDataSource, LoadRow};
use crate::analytics::types::QueryExecutionError;
pub type QueryResult<T> = error_stack::Result<T, QueryBuildingError>;
pub trait QueryFilter<T>
where
    T: AnalyticsDataSource,
    AnalyticsCollection: ToSql<T>,
{
    fn set_filter_clause(&self, builder: &mut QueryBuilder<T>) -> QueryResult<()>;
}

pub trait GroupByClause<T>
where
    T: AnalyticsDataSource,
    AnalyticsCollection: ToSql<T>,
{
    fn set_group_by_clause(&self, builder: &mut QueryBuilder<T>) -> QueryResult<()>;
}

pub trait SeriesBucket {
    type SeriesType;
    type GranularityLevel;

    fn get_lowest_common_granularity_level(&self) -> Self::GranularityLevel;

    fn get_bucket_size(&self) -> u8;

    fn clip_to_start(
        &self,
        value: Self::SeriesType,
    ) -> error_stack::Result<Self::SeriesType, PostProcessingError>;

    fn clip_to_end(
        &self,
        value: Self::SeriesType,
    ) -> error_stack::Result<Self::SeriesType, PostProcessingError>;
}

impl<T> QueryFilter<T> for analytics_api::TimeRange
where
    T: AnalyticsDataSource,
    time::PrimitiveDateTime: ToSql<T>,
    AnalyticsCollection: ToSql<T>,
    Granularity: GroupByClause<T>,
{
    fn set_filter_clause(&self, builder: &mut QueryBuilder<T>) -> QueryResult<()> {
        builder.add_custom_filter_clause("created_at", self.start_time, FilterTypes::Gte)?;
        if let Some(end) = self.end_time {
            builder.add_custom_filter_clause("created_at", end, FilterTypes::Lte)?;
        }
        Ok(())
    }
}

impl GroupByClause<super::SqlxClient> for Granularity {
    fn set_group_by_clause(
        &self,
        builder: &mut QueryBuilder<super::SqlxClient>,
    ) -> QueryResult<()> {
        let trunc_scale = self.get_lowest_common_granularity_level();

        let granularity_bucket_scale = match self {
            Self::OneMin => None,
            Self::FiveMin | Self::FifteenMin | Self::ThirtyMin => Some("minute"),
            Self::OneHour | Self::OneDay => None,
        };

        let granularity_divisor = self.get_bucket_size();

        builder
            .add_group_by_clause(format!("DATE_TRUNC('{trunc_scale}', modified_at)"))
            .attach_printable("Error adding time prune group by")?;
        if let Some(scale) = granularity_bucket_scale {
            builder
                .add_group_by_clause(format!(
                    "FLOOR(DATE_PART('{scale}', modified_at)/{granularity_divisor})"
                ))
                .attach_printable("Error adding time binning group by")?;
        }
        Ok(())
    }
}

#[derive(strum::Display)]
#[strum(serialize_all = "lowercase")]
pub enum TimeGranularityLevel {
    Minute,
    Hour,
    Day,
}

impl SeriesBucket for Granularity {
    type SeriesType = time::PrimitiveDateTime;

    type GranularityLevel = TimeGranularityLevel;

    fn get_lowest_common_granularity_level(&self) -> Self::GranularityLevel {
        match self {
            Self::OneMin => TimeGranularityLevel::Minute,
            Self::FiveMin | Self::FifteenMin | Self::ThirtyMin | Self::OneHour => {
                TimeGranularityLevel::Hour
            }
            Self::OneDay => TimeGranularityLevel::Day,
        }
    }

    fn get_bucket_size(&self) -> u8 {
        match self {
            Self::OneMin => 60,
            Self::FiveMin => 5,
            Self::FifteenMin => 15,
            Self::ThirtyMin => 30,
            Self::OneHour => 60,
            Self::OneDay => 24,
        }
    }

    fn clip_to_start(
        &self,
        value: Self::SeriesType,
    ) -> error_stack::Result<Self::SeriesType, PostProcessingError> {
        let clip_start = |value: u8, modulo: u8| -> u8 { value - value % modulo };

        let clipped_time = match (
            self.get_lowest_common_granularity_level(),
            self.get_bucket_size(),
        ) {
            (TimeGranularityLevel::Minute, i) => time::Time::MIDNIGHT
                .replace_second(clip_start(value.second(), i))
                .and_then(|t| t.replace_minute(value.minute()))
                .and_then(|t| t.replace_hour(value.hour())),
            (TimeGranularityLevel::Hour, i) => time::Time::MIDNIGHT
                .replace_minute(clip_start(value.minute(), i))
                .and_then(|t| t.replace_hour(value.hour())),
            (TimeGranularityLevel::Day, i) => {
                time::Time::MIDNIGHT.replace_hour(clip_start(value.hour(), i))
            }
        }
        .into_report()
        .change_context(PostProcessingError::BucketClipping)?;

        Ok(value.replace_time(clipped_time))
    }

    fn clip_to_end(
        &self,
        value: Self::SeriesType,
    ) -> error_stack::Result<Self::SeriesType, PostProcessingError> {
        let clip_end = |value: u8, modulo: u8| -> u8 { value + modulo - 1 - value % modulo };

        let clipped_time = match (
            self.get_lowest_common_granularity_level(),
            self.get_bucket_size(),
        ) {
            (TimeGranularityLevel::Minute, i) => time::Time::MIDNIGHT
                .replace_second(clip_end(value.second(), i))
                .and_then(|t| t.replace_minute(value.minute()))
                .and_then(|t| t.replace_hour(value.hour())),
            (TimeGranularityLevel::Hour, i) => time::Time::MIDNIGHT
                .replace_minute(clip_end(value.minute(), i))
                .and_then(|t| t.replace_hour(value.hour())),
            (TimeGranularityLevel::Day, i) => {
                time::Time::MIDNIGHT.replace_hour(clip_end(value.hour(), i))
            }
        }
        .into_report()
        .change_context(PostProcessingError::BucketClipping)
        .attach_printable_lazy(|| format!("Bucket Clip Error: {value}"))?;

        Ok(value.replace_time(clipped_time))
    }
}

#[derive(thiserror::Error, Debug)]
pub enum QueryBuildingError {
    #[allow(dead_code)]
    #[error("Not Implemented: {0}")]
    NotImplemented(String),
    #[error("Failed to Serialize to SQL")]
    SqlSerializeError,
    #[error("Failed to build sql query: {0}")]
    InvalidQuery(&'static str),
}

#[derive(thiserror::Error, Debug)]
pub enum PostProcessingError {
    #[error("Error Clipping values to bucket sizes")]
    BucketClipping,
}

#[derive(Debug)]
pub enum Aggregate<R> {
    Count {
        field: Option<R>,
        alias: Option<&'static str>,
    },
    Sum {
        field: R,
        alias: Option<&'static str>,
    },
    Min {
        field: R,
        alias: Option<&'static str>,
    },
    Max {
        field: R,
        alias: Option<&'static str>,
    },
}

#[derive(Debug)]
pub struct QueryBuilder<T>
where
    T: AnalyticsDataSource,
    AnalyticsCollection: ToSql<T>,
{
    columns: Vec<String>,
    filters: Vec<(String, FilterTypes, String)>,
    group_by: Vec<String>,
    having: Option<Vec<(String, FilterTypes, String)>>,
    table: AnalyticsCollection,
    distinct: bool,
    db_type: PhantomData<T>,
}

pub trait ToSql<T: AnalyticsDataSource> {
    fn to_sql(&self) -> error_stack::Result<String, ParsingError>;
}

/// Implement `ToSql` on arrays of types that impl `ToString`.
macro_rules! impl_to_sql_for_to_string {
    ($($type:ty),+) => {
        $(
            impl<T: AnalyticsDataSource> ToSql<T> for $type {
                fn to_sql(&self) -> error_stack::Result<String, ParsingError> {
                    Ok(self.to_string())
                }
            }
        )+
     };
}

impl_to_sql_for_to_string!(
    String,
    &str,
    &PaymentDimensions,
    &RefundDimensions,
    PaymentDimensions,
    RefundDimensions,
    PaymentMethod,
    AuthenticationType,
    Connector,
    AttemptStatus,
    RefundStatus,
    storage_enums::RefundStatus,
    Currency,
    RefundType,
    &String,
    &bool,
    &u64
);

#[allow(dead_code)]
#[derive(Debug)]
pub enum FilterTypes {
    Equal,
    EqualBool,
    In,
    Gte,
    Lte,
    Gt,
}

impl<T> QueryBuilder<T>
where
    T: AnalyticsDataSource,
    AnalyticsCollection: ToSql<T>,
{
    pub fn new(table: AnalyticsCollection) -> Self {
        Self {
            columns: Default::default(),
            filters: Default::default(),
            group_by: Default::default(),
            having: Default::default(),
            table,
            distinct: Default::default(),
            db_type: Default::default(),
        }
    }

    pub fn add_select_column(&mut self, column: impl ToSql<T>) -> QueryResult<()> {
        self.columns.push(
            column
                .to_sql()
                .change_context(QueryBuildingError::SqlSerializeError)
                .attach_printable("Error serializing select column")?,
        );
        Ok(())
    }

    pub fn set_distinct(&mut self) {
        self.distinct = true
    }

    pub fn add_filter_clause(
        &mut self,
        key: impl ToSql<T>,
        value: impl ToSql<T>,
    ) -> QueryResult<()> {
        self.add_custom_filter_clause(key, value, FilterTypes::Equal)
    }

    pub fn add_bool_filter_clause(
        &mut self,
        key: impl ToSql<T>,
        value: impl ToSql<T>,
    ) -> QueryResult<()> {
        self.add_custom_filter_clause(key, value, FilterTypes::EqualBool)
    }

    pub fn add_custom_filter_clause(
        &mut self,
        lhs: impl ToSql<T>,
        rhs: impl ToSql<T>,
        comparison: FilterTypes,
    ) -> QueryResult<()> {
        self.filters.push((
            lhs.to_sql()
                .change_context(QueryBuildingError::SqlSerializeError)
                .attach_printable("Error serializing filter key")?,
            comparison,
            rhs.to_sql()
                .change_context(QueryBuildingError::SqlSerializeError)
                .attach_printable("Error serializing filter value")?,
        ));
        Ok(())
    }

    pub fn add_filter_in_range_clause(
        &mut self,
        key: impl ToSql<T>,
        values: &[impl ToSql<T>],
    ) -> QueryResult<()> {
        let list = values
            .iter()
            .map(|i| {
                // trimming whitespaces from the filter values received in request, to prevent a possibility of an SQL injection
                i.to_sql().map(|s| {
                    let trimmed_str = s.replace(' ', "");
                    format!("'{trimmed_str}'")
                })
            })
            .collect::<error_stack::Result<Vec<String>, ParsingError>>()
            .change_context(QueryBuildingError::SqlSerializeError)
            .attach_printable("Error serializing range filter value")?
            .join(", ");
        self.add_custom_filter_clause(key, list, FilterTypes::In)
    }

    pub fn add_group_by_clause(&mut self, column: impl ToSql<T>) -> QueryResult<()> {
        self.group_by.push(
            column
                .to_sql()
                .change_context(QueryBuildingError::SqlSerializeError)
                .attach_printable("Error serializing group by field")?,
        );
        Ok(())
    }

    pub fn add_granularity_in_mins(&mut self, granularity: &Granularity) -> QueryResult<()> {
        let interval = match granularity {
            Granularity::OneMin => "1",
            Granularity::FiveMin => "5",
            Granularity::FifteenMin => "15",
            Granularity::ThirtyMin => "30",
            Granularity::OneHour => "60",
            Granularity::OneDay => "1440",
        };
        let _ = self.add_select_column(format!(
            "toStartOfInterval(created_at, INTERVAL {interval} MINUTE) as time_bucket"
        ));
        Ok(())
    }

    fn get_filter_clause(&self) -> String {
        self.filters
            .iter()
            .map(|(l, op, r)| match op {
                FilterTypes::EqualBool => format!("{l} = {r}"),
                FilterTypes::Equal => format!("{l} = '{r}'"),
                FilterTypes::In => format!("{l} IN ({r})"),
                FilterTypes::Gte => format!("{l} >= '{r}'"),
                FilterTypes::Gt => format!("{l} > {r}"),
                FilterTypes::Lte => format!("{l} <= '{r}'"),
            })
            .collect::<Vec<String>>()
            .join(" AND ")
    }

    fn get_select_clause(&self) -> String {
        self.columns.join(", ")
    }

    fn get_group_by_clause(&self) -> String {
        self.group_by.join(", ")
    }

    #[allow(dead_code)]
    pub fn add_having_clause<R>(
        &mut self,
        aggregate: Aggregate<R>,
        filter_type: FilterTypes,
        value: impl ToSql<T>,
    ) -> QueryResult<()>
    where
        Aggregate<R>: ToSql<T>,
    {
        let aggregate = aggregate
            .to_sql()
            .change_context(QueryBuildingError::SqlSerializeError)
            .attach_printable("Error serializing having aggregate")?;
        let value = value
            .to_sql()
            .change_context(QueryBuildingError::SqlSerializeError)
            .attach_printable("Error serializing having value")?;
        let entry = (aggregate, filter_type, value);
        if let Some(having) = &mut self.having {
            having.push(entry);
        } else {
            self.having = Some(vec![entry]);
        }
        Ok(())
    }

    pub fn get_filter_type_clause(&self) -> Option<String> {
        self.having.as_ref().map(|vec| {
            vec.iter()
                .map(|(l, op, r)| match op {
                    FilterTypes::Equal | FilterTypes::EqualBool => format!("{l} = {r}"),
                    FilterTypes::In => format!("{l} IN ({r})"),
                    FilterTypes::Gte => format!("{l} >= {r}"),
                    FilterTypes::Lte => format!("{l} < {r}"),
                    FilterTypes::Gt => format!("{l} > {r}"),
                })
                .collect::<Vec<String>>()
                .join(" AND ")
        })
    }

    pub fn build_query(&mut self) -> QueryResult<String>
    where
        Aggregate<&'static str>: ToSql<T>,
    {
        if self.columns.is_empty() {
            Err(QueryBuildingError::InvalidQuery(
                "No select fields provided",
            ))
            .into_report()?;
        }
        let mut query = String::from("SELECT ");

        if self.distinct {
            query.push_str("DISTINCT ");
        }

        query.push_str(&self.get_select_clause());

        query.push_str(" FROM ");

        query.push_str(
            &self
                .table
                .to_sql()
                .change_context(QueryBuildingError::SqlSerializeError)
                .attach_printable("Error serializing table value")?,
        );

        if !self.filters.is_empty() {
            query.push_str(" WHERE ");
            query.push_str(&self.get_filter_clause());
        }

        if !self.group_by.is_empty() {
            query.push_str(" GROUP BY ");
            query.push_str(&self.get_group_by_clause());
        }

        if self.having.is_some() {
            if let Some(condition) = self.get_filter_type_clause() {
                query.push_str(" HAVING ");
                query.push_str(condition.as_str());
            }
        }
        Ok(query)
    }

    pub async fn execute_query<R, P: AnalyticsDataSource>(
        &mut self,
        store: &P,
    ) -> CustomResult<CustomResult<Vec<R>, QueryExecutionError>, QueryBuildingError>
    where
        P: LoadRow<R>,
        Aggregate<&'static str>: ToSql<T>,
    {
        let query = self
            .build_query()
            .change_context(QueryBuildingError::SqlSerializeError)
            .attach_printable("Failed to execute query")?;
        logger::debug!(?query);
        Ok(store.load_results(query.as_str()).await)
    }
}
