use std::{fs::File, io::Read};

use csv::ReaderBuilder;
use linfa::prelude::*;
use linfa_linear::LinearRegression;
use ndarray::{array, Array2};
use ndarray_csv::{Array2Reader, ReadError};

pub struct Model {
	inner: linfa_linear::FittedLinearRegression<f64>,
}

impl Model {
	pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
		let data = array_from_csv(File::open("data/features.csv")?, false, b',')?;
		let targets = array_from_csv(File::open("data/targets.csv")?, false, b',')?
			.column(0)
			.to_owned();

		let feature_names = vec![
			"latency",
			"packet_loss_rate",
			"available_bandwidth",
			"residual_bandwidth",
			"node_uptime",
			"node_availability",
			"node_load",
			"node_utilization",
			"realtime_transfer_rate",
		];

		let dataset = Dataset::new(data, targets).with_feature_names(feature_names);

		// lin reg model creation
		let model = LinearRegression::default();

		// fit model to dataset
		Ok(Self {
			inner: model.fit(&dataset)?,
		})
	}

	/// `latency`, `packet_loss_rate`, `available_bandwidth`, `residual_bandwidth`,
	/// `node_uptime`, `node_availability`, `node_load`, `node_utilization`,
	/// `realtime_transfer_rate`
	pub fn predict(&self, data: [f64; 9]) -> f64 {
		let y = self.inner.predict(array![[
			data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8]
		]]);

		y.targets[0]
	}
}

pub fn array_from_csv<R: Read>(
	csv: R,
	has_headers: bool,
	separator: u8,
) -> Result<Array2<f64>, ReadError> {
	// parse CSV
	let mut reader = ReaderBuilder::new()
		.has_headers(has_headers)
		.delimiter(separator)
		.from_reader(csv);

	// extract ndarray
	reader.deserialize_array2_dynamic()
}
