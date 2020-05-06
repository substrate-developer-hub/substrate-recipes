//! A demonstration of an offchain worker that submits onchain callbacks

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(test)]
mod tests;

use frame_support::{
	debug,
	dispatch::DispatchResult, decl_module, decl_storage, decl_event, decl_error,
	weights::SimpleDispatchInfo,
};
use parity_scale_codec::{Encode, Decode};
use core::{fmt, convert::TryInto};

use frame_system::{self as system, ensure_signed, ensure_none, offchain};
use sp_core::crypto::KeyTypeId;
use sp_runtime::{
	offchain as rt_offchain,
	offchain::{storage::StorageValueRef},
	transaction_validity::{
		InvalidTransaction, ValidTransaction, TransactionValidity, TransactionSource
	},
};
use sp_std::prelude::*;
use sp_std::str as str;

// We use `alt_serde`, and Xanewok-modified `serde_json` so that we can compile the program
//   with serde(features `std`) and alt_serde(features `no_std`).
use alt_serde::{Deserialize, Deserializer};

/// Defines application identifier for crypto keys of this module.
///
/// Every module that deals with signatures needs to declare its unique identifier for
/// its crypto keys.
/// When offchain worker is signing transactions it's going to request keys of type
/// `KeyTypeId` from the keystore and use the ones it finds to sign the transaction.
/// The keys can be inserted manually via RPC (see `author_insertKey`).
pub const KEY_TYPE: KeyTypeId = KeyTypeId(*b"demo");
pub const NUM_VEC_LEN: usize = 10;

// We are fetching information from github public API about organisation `substrate-developer-hub`.
pub const HTTP_REMOTE_REQUEST_BYTES: &[u8] = b"https://api.github.com/orgs/substrate-developer-hub";
pub const HTTP_HEADER_USER_AGENT: &[u8] = b"jimmychu0807";

/// Based on the above `KeyTypeId` we need to generate a pallet-specific crypto type wrappers.
/// We can use from supported crypto kinds (`sr25519`, `ed25519` and `ecdsa`) and augment
/// the types with this pallet-specific identifier.
pub mod crypto {
	use crate::KEY_TYPE;
	use sp_runtime::app_crypto::{app_crypto, sr25519};
	app_crypto!(sr25519, KEY_TYPE);
}

// Specifying serde path as `alt_serde`
// ref: https://serde.rs/container-attrs.html#crate
#[serde(crate = "alt_serde")]
#[derive(Deserialize, Encode, Decode, Default)]
struct GithubInfo {
	// Specify our own deserializing function to convert JSON string to vector of bytes
	#[serde(deserialize_with = "de_string_to_bytes")]
	login: Vec<u8>,
	#[serde(deserialize_with = "de_string_to_bytes")]
	blog: Vec<u8>,
	public_repos: u32,
}

pub fn de_string_to_bytes<'de, D>(de: D) -> Result<Vec<u8>, D::Error>
where D: Deserializer<'de> {
	let s: &str = Deserialize::deserialize(de)?;
	Ok(s.as_bytes().to_vec())
}

impl fmt::Debug for GithubInfo {
	// `fmt` converts the vector of bytes inside the struct back to string for
	//   more friendly display.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{{ login: {}, blog: {}, public_repos: {} }}",
			str::from_utf8(&self.login).unwrap(),
			str::from_utf8(&self.blog).unwrap(),
			&self.public_repos
    	)
    }
}

/// This is the pallet's configuration trait
pub trait Trait: system::Trait {
	/// The overarching dispatch call type.
	type Call: From<Call<Self>>;
	/// The overarching event type.
	type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;
	/// The type to sign and submit transactions.
	type SubmitSignedTransaction: offchain::SubmitSignedTransaction<Self, <Self as Trait>::Call>;
	/// The type to submit unsigned transactions.
	type SubmitUnsignedTransaction: offchain::SubmitUnsignedTransaction<Self, <Self as Trait>::Call>;
}

// Custom data type
#[derive(Debug)]
enum TransactionType {
	SignedSubmitNumber,
	UnsignedSubmitNumber,
	HttpFetching,
	None,
}

decl_storage! {
	trait Store for Module<T: Trait> as Example {
		/// A vector of recently submitted numbers. Should be bounded
		Numbers get(fn numbers): Vec<u64>;
	}
}

decl_event!(
	/// Events generated by the module.
	pub enum Event<T> where AccountId = <T as system::Trait>::AccountId {
		/// Event generated when a new number is accepted to contribute to the average.
		NewNumber(Option<AccountId>, u64),
	}
);

decl_error! {
	pub enum Error for Module<T: Trait> {
		// Error returned when making signed transactions in off-chain worker
		SignedSubmitNumberError,
		// Error returned when making unsigned transactions in off-chain worker
		UnsignedSubmitNumberError,
		// Error returned when making remote http fetching
		HttpFetchingError,
	}
}

decl_module! {
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
		fn deposit_event() = default;

		#[weight = SimpleDispatchInfo::default()]
		pub fn submit_number_signed(origin, number: u64) -> DispatchResult {
			debug::info!("submit_number_signed: {:?}", number);
			let who = ensure_signed(origin)?;
			Self::append_or_replace_number(Some(who), number)
		}

		#[weight = SimpleDispatchInfo::default()]
		pub fn submit_number_unsigned(origin, _block: T::BlockNumber, number: u64) -> DispatchResult {
			debug::info!("submit_number_unsigned: {:?}", number);
			let _ = ensure_none(origin)?;
			Self::append_or_replace_number(None, number)
		}

		fn offchain_worker(block_number: T::BlockNumber) {
			debug::info!("Entering off-chain workers");

			let result = match Self::choose_tx_type(block_number) {
				TransactionType::SignedSubmitNumber => Self::signed_submit_number(block_number),
				TransactionType::UnsignedSubmitNumber => Self::unsigned_submit_number(block_number),
				TransactionType::HttpFetching => Self::fetch_if_needed(),
				TransactionType::None => Ok(())
			};

			if let Err(e) = result { debug::error!("Error: {:?}", e); }
		}
	}
}

impl<T: Trait> Module<T> {
	/// Add a new number to the list.
	fn append_or_replace_number(who: Option<T::AccountId>, number: u64) -> DispatchResult {
		Numbers::mutate(|numbers| {
			// The append or replace logic. The `numbers` vector is at most `NUM_VEC_LEN` long.
			let num_len = numbers.len();

			if num_len < NUM_VEC_LEN {
				numbers.push(number);
			} else {
				numbers[num_len % NUM_VEC_LEN] = number;
			}

			// displaying the average
			let average = match num_len {
				0 => 0,
				_ => numbers.iter().fold(0, {|acc, num| acc + num}) / (num_len as u64),
			};

			debug::info!("Current average of numbers is: {}", average);
		});

		// Raise the NewNumber event
		Self::deposit_event(RawEvent::NewNumber(who, number));
		Ok(())
	}

	fn choose_tx_type(block_number: T::BlockNumber) -> TransactionType {
		// Decide what type of transaction to submit based on block number.
		// Each block the offchain worker will submit one type of transaction back to the chain.
		// First a signed transaction, then an unsigned transaction, then an http fetch and json parsing.
		match block_number.try_into().ok().unwrap() % 3 {
			0 => TransactionType::SignedSubmitNumber,
			1 => TransactionType::UnsignedSubmitNumber,
			2 => TransactionType::HttpFetching,
			_ => TransactionType::None,
		}
	}

	/// Check if we have fetched github info before. If yes, we use the cached version that is
	///   stored in off-chain worker storage `storage`. If no, we fetch the remote info and then
	///   write the info into the storage for future retrieval.
	fn fetch_if_needed() -> Result<(), Error<T>> {

		// Start off by creating a reference to Local Storage value.
		// Since the local storage is common for all offchain workers, it's a good practice
		// to prepend our entry with the pallet name.
		let storage = StorageValueRef::persistent(b"offchain-demo::gh-info");

		// The local storage is persisted and shared between runs of the offchain workers,
		// and offchain workers may run concurrently. We can use the `mutate` function, to
		// write a storage entry in an atomic fashion.
		//
		// It has a similar API as `StorageValue` that offer `get`, `set`, `mutate`.
		// If we are using a get-check-set access pattern, we likely want to use `mutate` to access
		// the storage in one go.
		//
		// Ref: https://substrate.dev/rustdocs/v2.0.0-alpha.6/sp_runtime/offchain/storage/struct.StorageValueRef.html
		let res = storage.mutate(|store: Option<Option<GithubInfo>>| {
			match store {
				// info existed, returning the value
				Some(Some(info)) => {
					debug::info!("Using cached gh-info.");
					Ok(info)
				},
				// info not existed, so we remote fetch (and parse the JSON)
				_ => Self::fetch_n_parse(),
			}
		});

		// The value of `res` looks funny. Its type is `Result<Result<T, E>, E>`. The above
		// `mutate` function returns:function
		// `Ok(Ok(T))` - in case the value has been successfully set.
		// `Ok(Err(T))` - in case the value was returned, but could not been set in the storage.
		// `Err(_)` - in case the closure function returns an error.
		match res {
			Ok(Ok(gh_info)) => {
				// Print out our github info, whether it is newly-fetched or cached.
				debug::info!("gh-info: {:?}", gh_info);
				Ok(())
			},
			_ => Err(<Error<T>>::HttpFetchingError)
		}
	}

	/// Fetch from remote and deserialize the JSON to a struct
	fn fetch_n_parse() -> Result<GithubInfo, Error<T>> {
		let resp_bytes = Self::fetch_from_remote()
			.map_err(|e| {
				debug::error!("fetch_from_remote error: {:?}", e);
				<Error<T>>::HttpFetchingError
			})?;

		// Print out our fetched JSON string
		let resp_str = str::from_utf8(&resp_bytes)
			.map_err(|_| <Error<T>>::HttpFetchingError)?;
		debug::info!("{}", resp_str);

		// Deserializing JSON to struct, thanks to `serde` and `serde_derive`
		let gh_info: GithubInfo = serde_json::from_str(&resp_str).unwrap();
		Ok(gh_info)
	}

	/// This function uses the `offchain::http` API to query the remote github information,
	///   and returns the JSON response as vector of bytes.
	fn fetch_from_remote() -> Result<Vec<u8>, Error<T>> {
		let remote_url_bytes = HTTP_REMOTE_REQUEST_BYTES.to_vec();
		let user_agent = HTTP_HEADER_USER_AGENT.to_vec();
		let remote_url = str::from_utf8(&remote_url_bytes)
			.map_err(|_| <Error<T>>::HttpFetchingError)?;

		debug::info!("sending request to: {}", remote_url);

		// Initiate an external HTTP GET request. This is using high-level wrappers from `sp_runtime`.
		let request = rt_offchain::http::Request::get(remote_url);

		// Keeping the offchain worker execution time reasonable, so limiting the call to be within 3s.
		let timeout = sp_io::offchain::timestamp().add(rt_offchain::Duration::from_millis(3000));

		// For github API request, we also need to specify `user-agent` in http request header.
		//   See: https://developer.github.com/v3/#user-agent-required
		let pending = request
			.add_header("User-Agent", str::from_utf8(&user_agent)
				.map_err(|_| <Error<T>>::HttpFetchingError)?)
			.deadline(timeout) // Setting the timeout time
			.send() // Sending the request out by the host
			.map_err(|_| <Error<T>>::HttpFetchingError)?;

		// By default, the http request is async from the runtime perspective. So we are asking the
		//   runtime to wait here.
		// The returning value here is a `Result` of `Result`, so we are unwrapping it twice by two `?`
		//   ref: https://substrate.dev/rustdocs/master/sp_runtime/offchain/http/struct.PendingRequest.html#method.try_wait
		let response = pending.try_wait(timeout)
			.map_err(|_| <Error<T>>::HttpFetchingError)?
			.map_err(|_| <Error<T>>::HttpFetchingError)?;

		if response.code != 200 {
			debug::error!("Unexpected http request status code: {}", response.code);
			return Err(<Error<T>>::HttpFetchingError);
		}

		// Next we fully read the response body and collect it to a vector of bytes.
		Ok(response.body().collect::<Vec<u8>>())
	}

	fn signed_submit_number(block_number: T::BlockNumber) -> Result<(), Error<T>> {
		use offchain::SubmitSignedTransaction;
		if !T::SubmitSignedTransaction::can_sign() {
			debug::error!("No local account available");
			return Err(<Error<T>>::SignedSubmitNumberError);
		}

		// We are just submitting the current block number back on-chain
		let submission: u64 = block_number.try_into().ok().unwrap() as u64;
		let call = Call::submit_number_signed(submission);

		// Using `SubmitSignedTransaction` associated type we create and submit a transaction
		// representing the call, we've just created.
		// Submit signed will return a vector of results for all accounts that were found in the
		// local keystore with expected `KEY_TYPE`.
		let results = T::SubmitSignedTransaction::submit_signed(call);
		for (_acc, res) in &results {
			match res {
				Ok(()) => { debug::native::info!("off-chain send_signed: acc: {}| number: {}", _acc, submission); },
				Err(e) => {
					debug::error!("[{:?}] Failed in signed_submit_number: {:?}", _acc, e);
					return Err(<Error<T>>::SignedSubmitNumberError);
				}
			};
		}
		Ok(())
	}

	fn unsigned_submit_number(block_number: T::BlockNumber) -> Result<(), Error<T>> {
		use offchain::SubmitUnsignedTransaction;

		let submission: u64 = block_number.try_into().ok().unwrap() as u64;
		// Submitting the current block number back on-chain.
		// `blocknumber` and `submission` params are always the same value but in different
		//   data type. They seem redundant, but in reality they have different purposes.
		//   `submission` is the number to be recorded back on-chain. `block_number` is checked in
		//   `validate_unsigned` function so only one `Call::submit_number_unsigned` is accepted in
		//   each block generation phase.
		let call = Call::submit_number_unsigned(block_number, submission);

		T::SubmitUnsignedTransaction::submit_unsigned(call).map_err(|e| {
			debug::error!("Failed in unsigned_submit_number: {:?}", e);
			<Error<T>>::UnsignedSubmitNumberError
		})
	}
}

impl<T: Trait> frame_support::unsigned::ValidateUnsigned for Module<T> {
	type Call = Call<T>;

	fn validate_unsigned(
		_source: TransactionSource,
		call: &Self::Call
	) -> TransactionValidity {
		if let Call::submit_number_unsigned(block_num, number) = call {
			debug::native::info!("off-chain send_unsigned: block_num: {}| number: {}", block_num, number);

			Ok(ValidTransaction {
				priority: 1 << 20,
				requires: vec![],
				provides: vec![Encode::encode(&(KEY_TYPE.0, block_num))],
				longevity: 3,
				propagate: false,
			})
		} else {
			InvalidTransaction::Call.into()
		}
	}
}
