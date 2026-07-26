//! Quantitative analytics engine — spec v2.0 §6.
//!
//! Every known family of data-analytics methods relevant to telemetry,
//! predictive maintenance, and environmental monitoring is implemented
//! here as a deterministic computation. The AI layer (`ai.rs`) NEVER
//! invents numbers: it selects methods from this catalogue, runs them,
//! and narrates the results — every figure in an AI answer traces back
//! to a function in this module applied to on-chain-anchored readings.
//!
//! Catalogue:
//!
//! | Family | Methods |
//! |---|---|
//! | Descriptive statistics | count, sum, mean, median, mode, min/max, range, variance, stddev, coefficient of variation, percentiles, quartiles, IQR, skewness, kurtosis |
//! | Time series | time-bucketed resampling, SMA, EMA, rate of change, least-squares linear trend, autocorrelation, seasonality detection, decomposition (trend + seasonal + residual) |
//! | Anomaly detection | z-score, modified z-score (MAD), IQR fences, declared-band breaches, EWMA control limits, CUSUM drift |
//! | Forecasting | linear-trend extrapolation, Holt double exponential smoothing, Holt-Winters triple (additive seasonal) |
//! | Correlation | Pearson, Spearman rank, lagged cross-correlation, correlation matrix |
//! | Distribution | histogram, frequency table, normality assessment (Jarque-Bera statistic) |
//! | Clustering / segmentation | k-means (1-D), band segmentation |
//! | Comparative / cohort | group-by aggregation, top-N ranking, period-over-period delta |
//! | Predictive maintenance | degradation slope → remaining-useful-life, MTBF, failure rate, availability %, cadence conformity |
//! | Compliance | in-optimum %, breach counts by severity, SLA conformity report |
//! | Data quality | completeness, staleness, gap detection |

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ===========================================================================
// Descriptive statistics
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DescriptiveStats {
    pub count: usize,
    pub sum: f64,
    pub mean: f64,
    pub median: f64,
    pub mode: Option<f64>,
    pub min: f64,
    pub max: f64,
    pub range: f64,
    pub variance: f64,
    pub stddev: f64,
    /// Coefficient of variation (stddev / |mean|), 0 when mean == 0.
    pub cv: f64,
    pub p05: f64,
    pub p25: f64,
    pub p75: f64,
    pub p95: f64,
    pub iqr: f64,
    pub skewness: f64,
    /// Excess kurtosis (normal = 0).
    pub kurtosis: f64,
}

/// Percentile with linear interpolation (values need not be sorted).
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v: Vec<f64> = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p = p.clamp(0.0, 100.0) / 100.0;
    let idx = p * (v.len() as f64 - 1.0);
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        v[lo]
    } else {
        let frac = idx - lo as f64;
        v[lo] * (1.0 - frac) + v[hi] * frac
    }
}

pub fn describe(values: &[f64]) -> Option<DescriptiveStats> {
    if values.is_empty() {
        return None;
    }
    let n = values.len() as f64;
    let sum: f64 = values.iter().sum();
    let mean = sum / n;
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let variance = values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    let stddev = variance.sqrt();

    // Mode via 6-decimal binning (telemetry is finite-precision).
    let mut freq: BTreeMap<i64, (usize, f64)> = BTreeMap::new();
    for &x in values {
        let key = (x * 1e6).round() as i64;
        let e = freq.entry(key).or_insert((0, x));
        e.0 += 1;
    }
    let mode = freq
        .values()
        .max_by_key(|(c, _)| *c)
        .filter(|(c, _)| *c > 1)
        .map(|(_, v)| *v);

    let (skewness, kurtosis) = if stddev > 0.0 && values.len() > 2 {
        let m3 = values.iter().map(|x| ((x - mean) / stddev).powi(3)).sum::<f64>() / n;
        let m4 = values.iter().map(|x| ((x - mean) / stddev).powi(4)).sum::<f64>() / n;
        (m3, m4 - 3.0)
    } else {
        (0.0, 0.0)
    };

    let p25 = percentile(values, 25.0);
    let p75 = percentile(values, 75.0);

    Some(DescriptiveStats {
        count: values.len(),
        sum,
        mean,
        median: percentile(values, 50.0),
        mode,
        min,
        max,
        range: max - min,
        variance,
        stddev,
        cv: if mean.abs() > f64::EPSILON { stddev / mean.abs() } else { 0.0 },
        p05: percentile(values, 5.0),
        p25,
        p75,
        p95: percentile(values, 95.0),
        iqr: p75 - p25,
        skewness,
        kurtosis,
    })
}

// ===========================================================================
// Time series
// ===========================================================================

/// A single (timestamp, value) sample. Series passed to the functions
/// below must be sorted by `ts` ascending; [`sort_series`] does that.
pub type Sample = (i64, f64);

pub fn sort_series(series: &mut [Sample]) {
    series.sort_by_key(|(ts, _)| *ts);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bucket {
    pub ts_start: i64,
    pub count: usize,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
}

/// Resample a series into fixed-width time buckets (seconds).
pub fn resample(series: &[Sample], bucket_secs: i64) -> Vec<Bucket> {
    if series.is_empty() || bucket_secs <= 0 {
        return Vec::new();
    }
    let mut buckets: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for &(ts, v) in series {
        let start = (ts / bucket_secs) * bucket_secs;
        buckets.entry(start).or_default().push(v);
    }
    buckets
        .into_iter()
        .map(|(ts_start, vals)| {
            let count = vals.len();
            let sum: f64 = vals.iter().sum();
            Bucket {
                ts_start,
                count,
                mean: sum / count as f64,
                min: vals.iter().cloned().fold(f64::INFINITY, f64::min),
                max: vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                sum,
            }
        })
        .collect()
}

/// Simple moving average over a window of `w` samples.
pub fn sma(values: &[f64], w: usize) -> Vec<f64> {
    if w == 0 || values.len() < w {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(values.len() - w + 1);
    let mut sum: f64 = values[..w].iter().sum();
    out.push(sum / w as f64);
    for i in w..values.len() {
        sum += values[i] - values[i - w];
        out.push(sum / w as f64);
    }
    out
}

/// Exponential moving average with smoothing factor `alpha` in (0,1].
pub fn ema(values: &[f64], alpha: f64) -> Vec<f64> {
    if values.is_empty() {
        return Vec::new();
    }
    let alpha = alpha.clamp(f64::EPSILON, 1.0);
    let mut out = Vec::with_capacity(values.len());
    let mut prev = values[0];
    out.push(prev);
    for &v in &values[1..] {
        prev = alpha * v + (1.0 - alpha) * prev;
        out.push(prev);
    }
    out
}

/// Rate of change per hour between the first and last sample.
pub fn rate_of_change_per_hour(series: &[Sample]) -> Option<f64> {
    let (t0, v0) = *series.first()?;
    let (t1, v1) = *series.last()?;
    let dt_hours = (t1 - t0) as f64 / 3600.0;
    if dt_hours.abs() < f64::EPSILON {
        return None;
    }
    Some((v1 - v0) / dt_hours)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearTrend {
    /// Slope in value units per second.
    pub slope: f64,
    pub intercept: f64,
    /// Coefficient of determination.
    pub r_squared: f64,
    /// Slope expressed per hour, the human-friendly figure.
    pub slope_per_hour: f64,
    /// `rising` | `falling` | `stable` (|slope_per_hour| below 1e-9).
    pub direction: String,
}

/// Ordinary least-squares linear regression of value on time.
pub fn linear_trend(series: &[Sample]) -> Option<LinearTrend> {
    if series.len() < 2 {
        return None;
    }
    let n = series.len() as f64;
    let t0 = series[0].0 as f64;
    let mean_x = series.iter().map(|(t, _)| *t as f64 - t0).sum::<f64>() / n;
    let mean_y = series.iter().map(|(_, v)| *v).sum::<f64>() / n;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    let mut syy = 0.0;
    for &(t, v) in series {
        let dx = t as f64 - t0 - mean_x;
        let dy = v - mean_y;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    if sxx.abs() < f64::EPSILON {
        return None;
    }
    let slope = sxy / sxx;
    let intercept = mean_y - slope * mean_x;
    let r_squared = if syy.abs() < f64::EPSILON {
        1.0
    } else {
        (sxy * sxy) / (sxx * syy)
    };
    let slope_per_hour = slope * 3600.0;
    let direction = if slope_per_hour > 1e-9 {
        "rising"
    } else if slope_per_hour < -1e-9 {
        "falling"
    } else {
        "stable"
    };
    Some(LinearTrend {
        slope,
        intercept,
        r_squared,
        slope_per_hour,
        direction: direction.to_string(),
    })
}

/// Autocorrelation at a given lag (in samples).
pub fn autocorrelation(values: &[f64], lag: usize) -> Option<f64> {
    if lag == 0 || values.len() <= lag + 1 {
        return None;
    }
    let n = values.len();
    let mean = values.iter().sum::<f64>() / n as f64;
    let denom: f64 = values.iter().map(|v| (v - mean).powi(2)).sum();
    if denom.abs() < f64::EPSILON {
        return None;
    }
    let num: f64 = (0..n - lag)
        .map(|i| (values[i] - mean) * (values[i + lag] - mean))
        .sum();
    Some(num / denom)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seasonality {
    pub period: usize,
    pub strength: f64,
}

/// Detect the dominant seasonal period by scanning autocorrelation peaks
/// over lags 2..=max_lag. Returns None when no lag exceeds 0.3.
pub fn detect_seasonality(values: &[f64], max_lag: usize) -> Option<Seasonality> {
    let max_lag = max_lag.min(values.len().saturating_sub(2));
    let mut best: Option<Seasonality> = None;
    for lag in 2..=max_lag {
        if let Some(ac) = autocorrelation(values, lag) {
            if ac > 0.3 && best.as_ref().map(|b| ac > b.strength).unwrap_or(true) {
                best = Some(Seasonality { period: lag, strength: ac });
            }
        }
    }
    best
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decomposition {
    pub trend: Vec<f64>,
    pub seasonal: Vec<f64>,
    pub residual: Vec<f64>,
    pub period: usize,
}

/// Classical additive decomposition: centered-MA trend, mean seasonal
/// profile per phase, residual = value − trend − seasonal.
pub fn decompose(values: &[f64], period: usize) -> Option<Decomposition> {
    if period < 2 || values.len() < period * 2 {
        return None;
    }
    let n = values.len();
    // Centered moving average as the trend estimate.
    let half = period / 2;
    let mut trend = vec![f64::NAN; n];
    for i in half..n - half {
        let window = &values[i - half..=i + half.min(n - 1 - i)];
        trend[i] = window.iter().sum::<f64>() / window.len() as f64;
    }
    // Fill edges with nearest valid trend.
    let first_valid = trend.iter().position(|v| !v.is_nan()).unwrap_or(0);
    let last_valid = trend.iter().rposition(|v| !v.is_nan()).unwrap_or(n - 1);
    for i in 0..first_valid {
        trend[i] = trend[first_valid];
    }
    for i in last_valid + 1..n {
        trend[i] = trend[last_valid];
    }
    // Seasonal profile: mean detrended value per phase.
    let mut phase_sum = vec![0.0; period];
    let mut phase_cnt = vec![0usize; period];
    for i in 0..n {
        let detr = values[i] - trend[i];
        phase_sum[i % period] += detr;
        phase_cnt[i % period] += 1;
    }
    let mut profile: Vec<f64> = phase_sum
        .iter()
        .zip(&phase_cnt)
        .map(|(s, c)| if *c > 0 { s / *c as f64 } else { 0.0 })
        .collect();
    // Normalize so seasonal component sums to ~0.
    let profile_mean = profile.iter().sum::<f64>() / period as f64;
    for p in &mut profile {
        *p -= profile_mean;
    }
    let seasonal: Vec<f64> = (0..n).map(|i| profile[i % period]).collect();
    let residual: Vec<f64> = (0..n)
        .map(|i| values[i] - trend[i] - seasonal[i])
        .collect();
    Some(Decomposition { trend, seasonal, residual, period })
}

// ===========================================================================
// Anomaly detection
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    pub index: usize,
    pub ts: i64,
    pub value: f64,
    pub score: f64,
    pub method: String,
}

/// Z-score outliers: |z| > threshold (conventional threshold 3.0).
pub fn zscore_anomalies(series: &[Sample], threshold: f64) -> Vec<Anomaly> {
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    let Some(stats) = describe(&values) else {
        return Vec::new();
    };
    if stats.stddev < f64::EPSILON {
        return Vec::new();
    }
    series
        .iter()
        .enumerate()
        .filter_map(|(i, &(ts, v))| {
            let z = (v - stats.mean) / stats.stddev;
            (z.abs() > threshold).then(|| Anomaly {
                index: i,
                ts,
                value: v,
                score: z,
                method: "zscore".to_string(),
            })
        })
        .collect()
}

/// Modified z-score using the median absolute deviation — robust to the
/// outliers it is hunting. Conventional threshold 3.5.
pub fn mad_anomalies(series: &[Sample], threshold: f64) -> Vec<Anomaly> {
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    if values.is_empty() {
        return Vec::new();
    }
    let median = percentile(&values, 50.0);
    let deviations: Vec<f64> = values.iter().map(|v| (v - median).abs()).collect();
    let mad = percentile(&deviations, 50.0);
    if mad < f64::EPSILON {
        return Vec::new();
    }
    series
        .iter()
        .enumerate()
        .filter_map(|(i, &(ts, v))| {
            let mz = 0.6745 * (v - median) / mad;
            (mz.abs() > threshold).then(|| Anomaly {
                index: i,
                ts,
                value: v,
                score: mz,
                method: "modified_zscore_mad".to_string(),
            })
        })
        .collect()
}

/// Tukey IQR fences: outside [Q1 − k·IQR, Q3 + k·IQR] (k = 1.5 mild, 3.0 extreme).
pub fn iqr_anomalies(series: &[Sample], k: f64) -> Vec<Anomaly> {
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    if values.len() < 4 {
        return Vec::new();
    }
    let q1 = percentile(&values, 25.0);
    let q3 = percentile(&values, 75.0);
    let iqr = q3 - q1;
    let lo = q1 - k * iqr;
    let hi = q3 + k * iqr;
    series
        .iter()
        .enumerate()
        .filter_map(|(i, &(ts, v))| {
            (v < lo || v > hi).then(|| Anomaly {
                index: i,
                ts,
                value: v,
                score: if v < lo { (lo - v) / iqr.max(f64::EPSILON) } else { (v - hi) / iqr.max(f64::EPSILON) },
                method: "iqr_fence".to_string(),
            })
        })
        .collect()
}

/// EWMA control chart: flag samples outside center ± L·sigma_ewma.
/// Standard parameters: lambda = 0.2, L = 3.
pub fn ewma_control_anomalies(series: &[Sample], lambda: f64, l: f64) -> Vec<Anomaly> {
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    let Some(stats) = describe(&values) else {
        return Vec::new();
    };
    if stats.stddev < f64::EPSILON {
        return Vec::new();
    }
    let lambda = lambda.clamp(0.01, 1.0);
    let mut out = Vec::new();
    let mut z = stats.mean;
    for (i, &(ts, v)) in series.iter().enumerate() {
        z = lambda * v + (1.0 - lambda) * z;
        let denom = (lambda / (2.0 - lambda)
            * (1.0 - (1.0 - lambda).powi(2 * (i as i32 + 1))))
        .sqrt();
        let sigma_z = stats.stddev * denom;
        if sigma_z > f64::EPSILON {
            let deviation = (z - stats.mean) / sigma_z;
            if deviation.abs() > l {
                out.push(Anomaly {
                    index: i,
                    ts,
                    value: v,
                    score: deviation,
                    method: "ewma_control".to_string(),
                });
            }
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CusumDrift {
    pub index: usize,
    pub ts: i64,
    pub direction: String,
    pub cumulative: f64,
}

/// One-sided CUSUM drift detection with slack `k_sigma` (in σ) and decision
/// threshold `h_sigma` (in σ). Standard: k = 0.5, h = 5.
pub fn cusum_drift(series: &[Sample], k_sigma: f64, h_sigma: f64) -> Vec<CusumDrift> {
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    let Some(stats) = describe(&values) else {
        return Vec::new();
    };
    if stats.stddev < f64::EPSILON {
        return Vec::new();
    }
    let k = k_sigma * stats.stddev;
    let h = h_sigma * stats.stddev;
    let mut s_hi = 0.0f64;
    let mut s_lo = 0.0f64;
    let mut out = Vec::new();
    for (i, &(ts, v)) in series.iter().enumerate() {
        s_hi = (s_hi + v - stats.mean - k).max(0.0);
        s_lo = (s_lo + stats.mean - v - k).max(0.0);
        if s_hi > h {
            out.push(CusumDrift {
                index: i,
                ts,
                direction: "upward".to_string(),
                cumulative: s_hi,
            });
            s_hi = 0.0;
        }
        if s_lo > h {
            out.push(CusumDrift {
                index: i,
                ts,
                direction: "downward".to_string(),
                cumulative: s_lo,
            });
            s_lo = 0.0;
        }
    }
    out
}

// ===========================================================================
// Forecasting
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Forecast {
    pub method: String,
    /// Forecast points as (ts, value).
    pub points: Vec<Sample>,
    /// Symmetric 95% interval half-width per point (grows with horizon).
    pub interval: Vec<f64>,
}

/// Linear-trend extrapolation over `horizon_steps` future steps of
/// `step_secs` each.
pub fn forecast_linear(series: &[Sample], horizon_steps: usize, step_secs: i64) -> Option<Forecast> {
    let trend = linear_trend(series)?;
    let (t_last, _) = *series.last()?;
    let t0 = series[0].0 as f64;
    // Residual stddev for the interval.
    let residuals: Vec<f64> = series
        .iter()
        .map(|&(t, v)| v - (trend.intercept + trend.slope * (t as f64 - t0)))
        .collect();
    let resid_sd = describe(&residuals).map(|s| s.stddev).unwrap_or(0.0);
    let mut points = Vec::with_capacity(horizon_steps);
    let mut interval = Vec::with_capacity(horizon_steps);
    for i in 1..=horizon_steps {
        let t = t_last + step_secs * i as i64;
        let v = trend.intercept + trend.slope * (t as f64 - t0);
        points.push((t, v));
        interval.push(1.96 * resid_sd * (1.0 + i as f64 / series.len() as f64).sqrt());
    }
    Some(Forecast {
        method: "linear_trend".to_string(),
        points,
        interval,
    })
}

/// Holt double exponential smoothing (level + trend).
/// alpha, beta in (0,1). Returns forecasts for `horizon_steps`.
pub fn forecast_holt(
    series: &[Sample],
    alpha: f64,
    beta: f64,
    horizon_steps: usize,
    step_secs: i64,
) -> Option<Forecast> {
    if series.len() < 3 {
        return None;
    }
    let alpha = alpha.clamp(0.01, 0.99);
    let beta = beta.clamp(0.01, 0.99);
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    let mut level = values[0];
    let mut trend = values[1] - values[0];
    let mut one_step_errors = Vec::new();
    for &v in &values[1..] {
        let forecast = level + trend;
        one_step_errors.push(v - forecast);
        let new_level = alpha * v + (1.0 - alpha) * (level + trend);
        trend = beta * (new_level - level) + (1.0 - beta) * trend;
        level = new_level;
    }
    let err_sd = describe(&one_step_errors).map(|s| s.stddev).unwrap_or(0.0);
    let (t_last, _) = *series.last()?;
    let mut points = Vec::with_capacity(horizon_steps);
    let mut interval = Vec::with_capacity(horizon_steps);
    for i in 1..=horizon_steps {
        points.push((t_last + step_secs * i as i64, level + trend * i as f64));
        interval.push(1.96 * err_sd * (i as f64).sqrt());
    }
    Some(Forecast {
        method: "holt_double_exponential".to_string(),
        points,
        interval,
    })
}

/// Holt-Winters triple exponential smoothing with additive seasonality.
/// Requires at least two full seasons of data.
pub fn forecast_holt_winters(
    series: &[Sample],
    period: usize,
    alpha: f64,
    beta: f64,
    gamma: f64,
    horizon_steps: usize,
    step_secs: i64,
) -> Option<Forecast> {
    if period < 2 || series.len() < period * 2 {
        return None;
    }
    let alpha = alpha.clamp(0.01, 0.99);
    let beta = beta.clamp(0.01, 0.99);
    let gamma = gamma.clamp(0.01, 0.99);
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();

    // Initialize level/trend from the first season, seasonals from
    // deviations of season 1 around its mean.
    let season1_mean = values[..period].iter().sum::<f64>() / period as f64;
    let season2_mean = values[period..period * 2].iter().sum::<f64>() / period as f64;
    let mut level = season1_mean;
    let mut trend = (season2_mean - season1_mean) / period as f64;
    let mut seasonals: Vec<f64> = values[..period].iter().map(|v| v - season1_mean).collect();

    let mut one_step_errors = Vec::new();
    for (i, &v) in values.iter().enumerate() {
        let s = seasonals[i % period];
        let forecast = level + trend + s;
        one_step_errors.push(v - forecast);
        let new_level = alpha * (v - s) + (1.0 - alpha) * (level + trend);
        trend = beta * (new_level - level) + (1.0 - beta) * trend;
        seasonals[i % period] = gamma * (v - new_level) + (1.0 - gamma) * s;
        level = new_level;
    }
    let err_sd = describe(&one_step_errors).map(|s| s.stddev).unwrap_or(0.0);
    let (t_last, _) = *series.last()?;
    let n = values.len();
    let mut points = Vec::with_capacity(horizon_steps);
    let mut interval = Vec::with_capacity(horizon_steps);
    for i in 1..=horizon_steps {
        let seasonal = seasonals[(n + i - 1) % period];
        points.push((
            t_last + step_secs * i as i64,
            level + trend * i as f64 + seasonal,
        ));
        interval.push(1.96 * err_sd * (i as f64).sqrt());
    }
    Some(Forecast {
        method: "holt_winters_additive".to_string(),
        points,
        interval,
    })
}

// ===========================================================================
// Correlation
// ===========================================================================

/// Pearson product-moment correlation of two equal-length series.
pub fn pearson(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.len() < 2 {
        return None;
    }
    let n = a.len() as f64;
    let ma = a.iter().sum::<f64>() / n;
    let mb = b.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut da = 0.0;
    let mut db = 0.0;
    for i in 0..a.len() {
        let xa = a[i] - ma;
        let xb = b[i] - mb;
        num += xa * xb;
        da += xa * xa;
        db += xb * xb;
    }
    let denom = (da * db).sqrt();
    if denom < f64::EPSILON {
        return None;
    }
    Some(num / denom)
}

fn ranks(values: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..values.len()).collect();
    idx.sort_by(|&i, &j| {
        values[i]
            .partial_cmp(&values[j])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out = vec![0.0; values.len()];
    let mut i = 0;
    while i < idx.len() {
        // Average ranks over ties.
        let mut j = i;
        while j + 1 < idx.len() && values[idx[j + 1]] == values[idx[i]] {
            j += 1;
        }
        let avg_rank = (i + j) as f64 / 2.0 + 1.0;
        for k in i..=j {
            out[idx[k]] = avg_rank;
        }
        i = j + 1;
    }
    out
}

/// Spearman rank correlation (tie-aware).
pub fn spearman(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.len() < 2 {
        return None;
    }
    pearson(&ranks(a), &ranks(b))
}

/// Cross-correlation of `a` against `b` shifted by `lag` samples
/// (positive lag = b leads a).
pub fn lag_correlation(a: &[f64], b: &[f64], lag: usize) -> Option<f64> {
    if lag >= a.len() || lag >= b.len() {
        return None;
    }
    pearson(&a[lag..], &b[..b.len() - lag])
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationEntry {
    pub a: String,
    pub b: String,
    pub pearson: f64,
    pub spearman: f64,
    pub n: usize,
}

/// Full correlation matrix across named, equal-time-bucketed series.
pub fn correlation_matrix(named: &[(String, Vec<f64>)]) -> Vec<CorrelationEntry> {
    let mut out = Vec::new();
    for i in 0..named.len() {
        for j in i + 1..named.len() {
            let (na, va) = &named[i];
            let (nb, vb) = &named[j];
            let n = va.len().min(vb.len());
            if n < 3 {
                continue;
            }
            let (pa, pb) = (&va[..n], &vb[..n]);
            if let (Some(p), Some(s)) = (pearson(pa, pb), spearman(pa, pb)) {
                out.push(CorrelationEntry {
                    a: na.clone(),
                    b: nb.clone(),
                    pearson: p,
                    spearman: s,
                    n,
                });
            }
        }
    }
    out
}

// ===========================================================================
// Distribution
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramBin {
    pub lo: f64,
    pub hi: f64,
    pub count: usize,
}

pub fn histogram(values: &[f64], bins: usize) -> Vec<HistogramBin> {
    if values.is_empty() || bins == 0 {
        return Vec::new();
    }
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let width = if (max - min).abs() < f64::EPSILON {
        1.0
    } else {
        (max - min) / bins as f64
    };
    let mut counts = vec![0usize; bins];
    for &v in values {
        let mut b = ((v - min) / width) as usize;
        if b >= bins {
            b = bins - 1;
        }
        counts[b] += 1;
    }
    counts
        .into_iter()
        .enumerate()
        .map(|(i, count)| HistogramBin {
            lo: min + width * i as f64,
            hi: min + width * (i + 1) as f64,
            count,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalityAssessment {
    /// Jarque-Bera statistic: n/6 · (S² + K²/4).
    pub jarque_bera: f64,
    /// JB < 6 ≈ consistent with normality at the 5% level (χ²(2) ≈ 5.99).
    pub likely_normal: bool,
    pub skewness: f64,
    pub kurtosis: f64,
}

pub fn normality(values: &[f64]) -> Option<NormalityAssessment> {
    let stats = describe(values)?;
    if values.len() < 8 {
        return None;
    }
    let n = values.len() as f64;
    let jb = n / 6.0 * (stats.skewness.powi(2) + stats.kurtosis.powi(2) / 4.0);
    Some(NormalityAssessment {
        jarque_bera: jb,
        likely_normal: jb < 5.99,
        skewness: stats.skewness,
        kurtosis: stats.kurtosis,
    })
}

// ===========================================================================
// Clustering / segmentation
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KMeans1D {
    pub centroids: Vec<f64>,
    /// Cluster index per input value.
    pub assignments: Vec<usize>,
    pub iterations: usize,
}

/// Deterministic 1-D k-means (centroids seeded at evenly spaced
/// percentiles, so results are reproducible run to run).
pub fn kmeans_1d(values: &[f64], k: usize, max_iter: usize) -> Option<KMeans1D> {
    if values.is_empty() || k == 0 || values.len() < k {
        return None;
    }
    let mut centroids: Vec<f64> = (0..k)
        .map(|i| percentile(values, 100.0 * (i as f64 + 0.5) / k as f64))
        .collect();
    let mut assignments = vec![0usize; values.len()];
    let mut iterations = 0;
    for _ in 0..max_iter {
        iterations += 1;
        let mut changed = false;
        for (i, &v) in values.iter().enumerate() {
            let (best, _) = centroids
                .iter()
                .enumerate()
                .map(|(c, &cv)| (c, (v - cv).abs()))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap();
            if assignments[i] != best {
                assignments[i] = best;
                changed = true;
            }
        }
        let mut sums = vec![0.0; k];
        let mut counts = vec![0usize; k];
        for (i, &v) in values.iter().enumerate() {
            sums[assignments[i]] += v;
            counts[assignments[i]] += 1;
        }
        for c in 0..k {
            if counts[c] > 0 {
                centroids[c] = sums[c] / counts[c] as f64;
            }
        }
        if !changed {
            break;
        }
    }
    Some(KMeans1D {
        centroids,
        assignments,
        iterations,
    })
}

// ===========================================================================
// Comparative / cohort
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupStat {
    pub group: String,
    pub count: usize,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
}

/// Group-by aggregation over (group_label, value) pairs.
pub fn group_by(rows: &[(String, f64)]) -> Vec<GroupStat> {
    let mut groups: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for (g, v) in rows {
        groups.entry(g.clone()).or_default().push(*v);
    }
    groups
        .into_iter()
        .map(|(group, vals)| {
            let count = vals.len();
            let sum: f64 = vals.iter().sum();
            GroupStat {
                group,
                count,
                mean: sum / count as f64,
                min: vals.iter().cloned().fold(f64::INFINITY, f64::min),
                max: vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                sum,
            }
        })
        .collect()
}

/// Top-N groups by a chosen statistic (`mean` | `sum` | `count` | `max` | `min`).
pub fn top_n(mut stats: Vec<GroupStat>, by: &str, n: usize, descending: bool) -> Vec<GroupStat> {
    stats.sort_by(|a, b| {
        let (xa, xb) = match by {
            "sum" => (a.sum, b.sum),
            "count" => (a.count as f64, b.count as f64),
            "max" => (a.max, b.max),
            "min" => (a.min, b.min),
            _ => (a.mean, b.mean),
        };
        if descending {
            xb.partial_cmp(&xa).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            xa.partial_cmp(&xb).unwrap_or(std::cmp::Ordering::Equal)
        }
    });
    stats.truncate(n);
    stats
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodComparison {
    pub current_mean: f64,
    pub previous_mean: f64,
    pub delta: f64,
    pub delta_pct: f64,
    pub current_count: usize,
    pub previous_count: usize,
}

/// Period-over-period comparison: [split_ts, ∞) vs (−∞, split_ts).
pub fn period_over_period(series: &[Sample], split_ts: i64) -> Option<PeriodComparison> {
    let (prev, cur): (Vec<f64>, Vec<f64>) = series.iter().fold(
        (Vec::new(), Vec::new()),
        |(mut p, mut c), &(ts, v)| {
            if ts < split_ts {
                p.push(v);
            } else {
                c.push(v);
            }
            (p, c)
        },
    );
    if prev.is_empty() || cur.is_empty() {
        return None;
    }
    let pm = prev.iter().sum::<f64>() / prev.len() as f64;
    let cm = cur.iter().sum::<f64>() / cur.len() as f64;
    let delta = cm - pm;
    Some(PeriodComparison {
        current_mean: cm,
        previous_mean: pm,
        delta,
        delta_pct: if pm.abs() > f64::EPSILON { delta / pm.abs() * 100.0 } else { 0.0 },
        current_count: cur.len(),
        previous_count: prev.len(),
    })
}

// ===========================================================================
// Predictive maintenance
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulEstimate {
    /// Hours until the degradation trend crosses the failure threshold.
    pub hours_remaining: f64,
    /// Timestamp when the threshold is projected to be crossed.
    pub projected_ts: i64,
    pub trend_slope_per_hour: f64,
    pub r_squared: f64,
    pub threshold: f64,
    pub current_value: f64,
}

/// Remaining-useful-life estimate: fit the degradation trend and project
/// when it crosses `failure_threshold`. Works in both directions (rising
/// toward an upper limit, e.g. vibration, or falling toward a lower one,
/// e.g. health score / lumen output).
pub fn remaining_useful_life(series: &[Sample], failure_threshold: f64) -> Option<RulEstimate> {
    let trend = linear_trend(series)?;
    let &(t_last, current) = series.last()?;
    let gap = failure_threshold - current;
    // The trend must actually be heading toward the threshold.
    if trend.slope.abs() < f64::EPSILON || gap.signum() != trend.slope.signum() {
        return None;
    }
    let secs = gap / trend.slope;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some(RulEstimate {
        hours_remaining: secs / 3600.0,
        projected_ts: t_last + secs as i64,
        trend_slope_per_hour: trend.slope_per_hour,
        r_squared: trend.r_squared,
        threshold: failure_threshold,
        current_value: current,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReliabilityStats {
    /// Mean time between failures, hours (None with < 2 failures).
    pub mtbf_hours: Option<f64>,
    /// Failures per 1000 operating hours.
    pub failure_rate_per_khour: f64,
    /// Availability = uptime / total window, percent.
    pub availability_pct: f64,
    pub failure_count: usize,
    pub window_hours: f64,
}

/// Compute reliability figures from failure event timestamps and total
/// accumulated downtime over an observation window.
pub fn reliability(
    failure_ts: &[i64],
    downtime_secs: i64,
    window_start: i64,
    window_end: i64,
) -> Option<ReliabilityStats> {
    if window_end <= window_start {
        return None;
    }
    let window_hours = (window_end - window_start) as f64 / 3600.0;
    let mut sorted = failure_ts.to_vec();
    sorted.sort_unstable();
    let mtbf = if sorted.len() >= 2 {
        let gaps: Vec<f64> = sorted
            .windows(2)
            .map(|w| (w[1] - w[0]) as f64 / 3600.0)
            .collect();
        Some(gaps.iter().sum::<f64>() / gaps.len() as f64)
    } else {
        None
    };
    let uptime = ((window_end - window_start) - downtime_secs).max(0) as f64;
    Some(ReliabilityStats {
        mtbf_hours: mtbf,
        failure_rate_per_khour: sorted.len() as f64 / window_hours * 1000.0,
        availability_pct: uptime / (window_end - window_start) as f64 * 100.0,
        failure_count: sorted.len(),
        window_hours,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CadenceConformity {
    pub expected: usize,
    pub received: usize,
    pub conformity_pct: f64,
    /// Gaps longer than 2× the expected interval, as (gap_start, gap_secs).
    pub gaps: Vec<(i64, i64)>,
}

/// How faithfully a sensor honored its declared cadence over a window.
pub fn cadence_conformity(
    series: &[Sample],
    expected_interval_secs: i64,
    window_start: i64,
    window_end: i64,
) -> Option<CadenceConformity> {
    if expected_interval_secs <= 0 || window_end <= window_start {
        return None;
    }
    let expected = ((window_end - window_start) / expected_interval_secs).max(1) as usize;
    let in_window: Vec<i64> = series
        .iter()
        .map(|(ts, _)| *ts)
        .filter(|ts| *ts >= window_start && *ts <= window_end)
        .collect();
    let mut gaps = Vec::new();
    for w in in_window.windows(2) {
        let gap = w[1] - w[0];
        if gap > expected_interval_secs * 2 {
            gaps.push((w[0], gap));
        }
    }
    Some(CadenceConformity {
        expected,
        received: in_window.len(),
        conformity_pct: (in_window.len() as f64 / expected as f64 * 100.0).min(100.0),
        gaps,
    })
}

// ===========================================================================
// Compliance
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceReport {
    pub total: usize,
    pub in_optimum: usize,
    pub warnings: usize,
    pub criticals: usize,
    pub in_optimum_pct: f64,
    /// SLA conformity against a target percentage of in-optimum readings.
    pub sla_target_pct: f64,
    pub sla_met: bool,
}

/// Compliance report over pre-classified reading bands
/// (`ok` / `warning` / `critical`) against an SLA target.
pub fn compliance_report(bands: &[String], sla_target_pct: f64) -> ComplianceReport {
    let total = bands.len();
    let in_optimum = bands.iter().filter(|b| b.as_str() == "ok").count();
    let warnings = bands.iter().filter(|b| b.as_str() == "warning").count();
    let criticals = bands.iter().filter(|b| b.as_str() == "critical").count();
    let pct = if total > 0 {
        in_optimum as f64 / total as f64 * 100.0
    } else {
        100.0
    };
    ComplianceReport {
        total,
        in_optimum,
        warnings,
        criticals,
        in_optimum_pct: pct,
        sla_target_pct,
        sla_met: pct >= sla_target_pct,
    }
}

// ===========================================================================
// Data quality
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataQuality {
    /// Received / expected readings, percent (capped at 100).
    pub completeness_pct: f64,
    /// Seconds since the most recent reading.
    pub staleness_secs: i64,
    pub gap_count: usize,
    pub longest_gap_secs: i64,
}

pub fn data_quality(
    series: &[Sample],
    expected_interval_secs: i64,
    window_start: i64,
    now: i64,
) -> Option<DataQuality> {
    let conf = cadence_conformity(series, expected_interval_secs, window_start, now)?;
    let last_ts = series.last().map(|(ts, _)| *ts).unwrap_or(window_start);
    Some(DataQuality {
        completeness_pct: conf.conformity_pct,
        staleness_secs: (now - last_ts).max(0),
        gap_count: conf.gaps.len(),
        longest_gap_secs: conf.gaps.iter().map(|(_, g)| *g).max().unwrap_or(0),
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn series_linear(n: usize, slope: f64) -> Vec<Sample> {
        (0..n).map(|i| (i as i64 * 3600, slope * i as f64)).collect()
    }

    #[test]
    fn descriptive_stats_basics() {
        let v = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let s = describe(&v).unwrap();
        assert_eq!(s.count, 8);
        assert!((s.mean - 5.0).abs() < 1e-9);
        assert!((s.stddev - 2.0).abs() < 1e-9);
        assert_eq!(s.mode, Some(4.0));
        assert!((s.median - 4.5).abs() < 1e-9);
    }

    #[test]
    fn linear_trend_exact() {
        let s = series_linear(10, 2.0);
        let t = linear_trend(&s).unwrap();
        assert!((t.slope_per_hour - 2.0).abs() < 1e-9);
        assert!((t.r_squared - 1.0).abs() < 1e-9);
        assert_eq!(t.direction, "rising");
    }

    #[test]
    fn sma_and_ema() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(sma(&v, 3), vec![2.0, 3.0, 4.0]);
        let e = ema(&v, 0.5);
        assert_eq!(e.len(), 5);
        assert!((e[1] - 1.5).abs() < 1e-9);
    }

    #[test]
    fn zscore_finds_spike() {
        // Small natural variation (MAD > 0) plus one gross outlier.
        let mut s: Vec<Sample> = (0..50)
            .map(|i| (i, 10.0 + (i % 5) as f64 * 0.1))
            .collect();
        s.push((50, 100.0));
        let mad = mad_anomalies(&s, 3.5);
        assert_eq!(mad.len(), 1);
        assert_eq!(mad[0].value, 100.0);
        let z = zscore_anomalies(&s, 3.0);
        assert_eq!(z.len(), 1);
        assert_eq!(z[0].value, 100.0);
    }

    #[test]
    fn iqr_fences() {
        let s: Vec<Sample> = vec![
            (0, 10.0), (1, 11.0), (2, 12.0), (3, 10.5),
            (4, 11.5), (5, 10.2), (6, 11.8), (7, 60.0),
        ];
        let out = iqr_anomalies(&s, 1.5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value, 60.0);
    }

    #[test]
    fn cusum_detects_shift() {
        let mut s: Vec<Sample> = (0..40).map(|i| (i, 10.0 + (i % 3) as f64 * 0.1)).collect();
        s.extend((40..80).map(|i| (i, 14.0 + (i % 3) as f64 * 0.1)));
        let drifts = cusum_drift(&s, 0.5, 4.0);
        // Against the global mean, the pre-shift segment registers as a
        // downward excursion and the post-shift segment as upward — both
        // must be present, and the upward one must land after the shift.
        assert!(!drifts.is_empty());
        let upward: Vec<_> = drifts.iter().filter(|d| d.direction == "upward").collect();
        assert!(!upward.is_empty(), "no upward drift detected: {drifts:?}");
        assert!(upward[0].index >= 40, "upward drift fired before the shift");
    }

    #[test]
    fn forecast_linear_extrapolates() {
        let s = series_linear(24, 1.0);
        let f = forecast_linear(&s, 3, 3600).unwrap();
        assert_eq!(f.points.len(), 3);
        assert!((f.points[0].1 - 24.0).abs() < 1e-6);
        assert!((f.points[2].1 - 26.0).abs() < 1e-6);
    }

    #[test]
    fn holt_winters_tracks_seasonal() {
        // Period-4 seasonal square wave on a rising base.
        let profile = [0.0, 5.0, 0.0, -5.0];
        let s: Vec<Sample> = (0..32)
            .map(|i| (i as i64 * 3600, i as f64 * 0.5 + profile[i % 4]))
            .collect();
        let f = forecast_holt_winters(&s, 4, 0.5, 0.3, 0.3, 4, 3600).unwrap();
        assert_eq!(f.points.len(), 4);
        // The seasonal swing must survive in the forecast: max - min > 5.
        let vals: Vec<f64> = f.points.iter().map(|(_, v)| *v).collect();
        let span = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            - vals.iter().cloned().fold(f64::INFINITY, f64::min);
        assert!(span > 5.0, "seasonal amplitude lost: span={span}");
    }

    #[test]
    fn correlations() {
        let a = [1.0, 2.0, 3.0, 4.0, 5.0];
        let b = [2.0, 4.0, 6.0, 8.0, 10.0];
        let c = [10.0, 8.0, 6.0, 4.0, 2.0];
        assert!((pearson(&a, &b).unwrap() - 1.0).abs() < 1e-9);
        assert!((pearson(&a, &c).unwrap() + 1.0).abs() < 1e-9);
        assert!((spearman(&a, &b).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn seasonality_detected() {
        let profile = [0.0, 10.0, 0.0, -10.0];
        let v: Vec<f64> = (0..40).map(|i| profile[i % 4]).collect();
        let s = detect_seasonality(&v, 10).unwrap();
        assert_eq!(s.period, 4);
        assert!(s.strength > 0.8);
    }

    #[test]
    fn decomposition_recovers_components() {
        let profile = [0.0, 4.0, 0.0, -4.0];
        let v: Vec<f64> = (0..40).map(|i| i as f64 * 0.1 + profile[i % 4]).collect();
        let d = decompose(&v, 4).unwrap();
        assert_eq!(d.trend.len(), 40);
        // Seasonal profile amplitude should be close to 4.
        let smax = d.seasonal.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(smax > 3.0);
    }

    #[test]
    fn kmeans_separates_clusters() {
        let v = [1.0, 1.1, 0.9, 10.0, 10.2, 9.8];
        let km = kmeans_1d(&v, 2, 50).unwrap();
        assert_eq!(km.centroids.len(), 2);
        let mut c = km.centroids.clone();
        c.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((c[0] - 1.0).abs() < 0.2);
        assert!((c[1] - 10.0).abs() < 0.2);
        // First three values share a cluster, last three share the other.
        assert_eq!(km.assignments[0], km.assignments[1]);
        assert_eq!(km.assignments[3], km.assignments[4]);
        assert_ne!(km.assignments[0], km.assignments[3]);
    }

    #[test]
    fn group_by_and_top_n() {
        let rows = vec![
            ("zone_a".to_string(), 10.0),
            ("zone_a".to_string(), 20.0),
            ("zone_b".to_string(), 5.0),
        ];
        let stats = group_by(&rows);
        assert_eq!(stats.len(), 2);
        let top = top_n(stats, "mean", 1, true);
        assert_eq!(top[0].group, "zone_a");
        assert!((top[0].mean - 15.0).abs() < 1e-9);
    }

    #[test]
    fn period_comparison() {
        let s: Vec<Sample> = (0..10)
            .map(|i| (i, if i < 5 { 10.0 } else { 20.0 }))
            .collect();
        let c = period_over_period(&s, 5).unwrap();
        assert!((c.previous_mean - 10.0).abs() < 1e-9);
        assert!((c.current_mean - 20.0).abs() < 1e-9);
        assert!((c.delta_pct - 100.0).abs() < 1e-9);
    }

    #[test]
    fn rul_projection() {
        // Vibration rising 0.5/hour, failure at 30, currently at 20 after 20h.
        let s: Vec<Sample> = (0..21).map(|i| (i as i64 * 3600, 10.0 + 0.5 * i as f64)).collect();
        let rul = remaining_useful_life(&s, 30.0).unwrap();
        assert!((rul.hours_remaining - 20.0).abs() < 1e-6);
    }

    #[test]
    fn rul_none_when_trend_diverges() {
        // Falling trend can never reach a higher threshold.
        let s: Vec<Sample> = (0..10).map(|i| (i as i64 * 3600, 100.0 - i as f64)).collect();
        assert!(remaining_useful_life(&s, 200.0).is_none());
    }

    #[test]
    fn reliability_stats() {
        let day = 86_400i64;
        let failures = vec![day, day * 3, day * 5];
        let r = reliability(&failures, 3600 * 6, 0, day * 10).unwrap();
        assert_eq!(r.failure_count, 3);
        assert!((r.mtbf_hours.unwrap() - 48.0).abs() < 1e-9);
        assert!(r.availability_pct > 97.0);
    }

    #[test]
    fn cadence_and_quality() {
        // Expected every 600s over 6000s → 10; only 5 received, one big gap.
        let s: Vec<Sample> = vec![
            (0, 1.0), (600, 1.0), (1200, 1.0), (4800, 1.0), (5400, 1.0),
        ];
        let c = cadence_conformity(&s, 600, 0, 6000).unwrap();
        assert_eq!(c.expected, 10);
        assert_eq!(c.received, 5);
        assert_eq!(c.gaps.len(), 1);
        let q = data_quality(&s, 600, 0, 6000).unwrap();
        assert_eq!(q.gap_count, 1);
        assert_eq!(q.staleness_secs, 600);
    }

    #[test]
    fn compliance_sla() {
        let bands: Vec<String> = ["ok", "ok", "ok", "warning", "critical"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let r = compliance_report(&bands, 50.0);
        assert_eq!(r.in_optimum, 3);
        assert!((r.in_optimum_pct - 60.0).abs() < 1e-9);
        assert!(r.sla_met);
        assert!(!compliance_report(&bands, 90.0).sla_met);
    }

    #[test]
    fn histogram_and_normality() {
        let v: Vec<f64> = (0..100).map(|i| (i % 10) as f64).collect();
        let h = histogram(&v, 10);
        assert_eq!(h.len(), 10);
        assert_eq!(h.iter().map(|b| b.count).sum::<usize>(), 100);
        assert!(normality(&v).is_some());
    }
}
