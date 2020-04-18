//! A demonstration of an offchain worker that submits onchain callbacks

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(test)]
mod tests;

use frame_support::{
	debug,
	dispatch::DispatchResult, decl_module, decl_storage, decl_event, decl_error,
	traits::Get,
	weights::SimpleDispatchInfo,
};

use core::convert::{TryInto};

use frame_system::{self as system, ensure_signed, ensure_none, offchain};
use sp_core::crypto::KeyTypeId;
use sp_runtime::{
	transaction_validity::{InvalidTransaction, ValidTransaction, TransactionValidity},
};
use sp_std::prelude::*;

/// Defines application identifier for crypto keys of this module.
///
/// Every module that deals with signatures needs to declare its unique identifier for
/// its crypto keys.
/// When offchain worker is signing transactions it's going to request keys of type
/// `KeyTypeId` from the keystore and use the ones it finds to sign the transaction.
/// The keys can be inserted manually via RPC (see `author_insertKey`).
pub const KEY_TYPE: KeyTypeId = KeyTypeId(*b"demo");
pub const NUM_VEC_LEN: usize = 10;

/// Based on the above `KeyTypeId` we need to generate a pallet-specific crypto type wrappers.
/// We can use from supported crypto kinds (`sr25519`, `ed25519` and `ecdsa`) and augment
/// the types with this pallet-specific identifier.
pub mod crypto {
	use crate::KEY_TYPE;
	use sp_runtime::app_crypto::{app_crypto, sr25519};
	app_crypto!(sr25519, KEY_TYPE);
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
	/// The period (specified in block time) during which new numbers will not be submitted
	type GracePeriod: Get<Self::BlockNumber>;
}

// Custom data type
#[derive(Debug)]
enum TransactionType {
	Signed,
	Unsigned,
	None,
}

decl_storage! {
	trait Store for Module<T: Trait> as Example {
		/// A vector of recently submitted numbers. Should be bounded
		Numbers get(fn numbers): Vec<u64>;
		/// Defines the block when next off-chain transaction will be accepted.
		NextTx get(fn next_tx): T::BlockNumber;
		/// How many transactions have been submitted by the offchain worker so far.
		AddSeq get(fn add_seq): u32;
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
		SendSignedError,
		// Error returned when making unsigned transactions in off-chain worker
		SendUnsignedError,
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

			let res = match Self::choose_tx_type(block_number) {
				TransactionType::Signed => Self::send_signed(block_number),
				TransactionType::Unsigned => Self::send_unsigned(block_number),
				TransactionType::None => Ok(())
			};

			if let Err(e) = res { debug::error!("Error: {:?}", e); }
		}
	}
}

impl<T: Trait> Module<T> {
	/// Add a new number to the list.
	fn append_or_replace_number(who: Option<T::AccountId>, number: u64) -> DispatchResult {
		let current_seq = Self::add_seq();
		Numbers::mutate(|numbers| {
			// The append or replace logic. The `numbers` vector is at most `NUM_VEC_LEN` long.
			if (current_seq as usize) < NUM_VEC_LEN {
				numbers.push(number);
			} else {
				numbers[current_seq as usize % NUM_VEC_LEN] = number;
			}

			// displaying the average
			let average = numbers.iter().fold(0, {|acc, num| acc + num}) / (numbers.len() as u64);
			debug::info!("Current average of numbers is: {}", average);
		});


		// Update the storage & seq for next update block
		<NextTx<T>>::mutate(|block| *block += T::GracePeriod::get());
		<AddSeq>::mutate(|seq| *seq += 1);

		// Raise the NewNumber event
		Self::deposit_event(RawEvent::NewNumber(who, number));
		Ok(())
	}

	fn choose_tx_type(block_number: T::BlockNumber) -> TransactionType {
		let next_tx = Self::next_tx();
		// Do not perform transaction if still within grace period
		if next_tx > block_number { return TransactionType::None; }

		if Self::add_seq() % 2 == 0 {
			TransactionType::Signed
		} else {
			TransactionType::Unsigned
		}
	}

	fn send_signed(block_number: T::BlockNumber) -> Result<(), Error<T>> {
		use system::offchain::SubmitSignedTransaction;
		if !T::SubmitSignedTransaction::can_sign() {
			debug::error!("No local account available");
			return Err(<Error<T>>::SendSignedError);
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
					debug::error!("[{:?}] Failed to submit signed tx: {:?}", _acc, e);
					return Err(<Error<T>>::SendSignedError);
				}
			};
		}
		Ok(())
	}

	fn send_unsigned(block_number: T::BlockNumber) -> Result<(), Error<T>> {
		use system::offchain::SubmitUnsignedTransaction;

		let submission: u64 = block_number.try_into().ok().unwrap() as u64;
		// Submitting the current block number back on-chain.
		// `blocknumber` and `submission` params are always the same value but in different
		//   data type. They seem redundant, but in reality they have different purposes.
		//   `submission` is the number to be recorded back on-chain. `block_number` is checked in
		//   `validate_unsigned` function so only one `Call::submit_number_unsigned` is accepted in
		//   each block generation phase.
		let call = Call::submit_number_unsigned(block_number, submission);

		T::SubmitUnsignedTransaction::submit_unsigned(call).map_err(|e| {
			debug::error!("Failed to submit unsigned tx: {:?}", e);
			<Error<T>>::SendUnsignedError
		})
	}
}

impl<T: Trait> frame_support::unsigned::ValidateUnsigned for Module<T> {
	type Call = Call<T>;

	fn validate_unsigned(call: &Self::Call) -> TransactionValidity {
		if let Call::submit_number_unsigned(block_num, number) = call {
			debug::native::info!("off-chain send_unsigned: block_num: {}| number: {}", block_num, number);

			Ok(ValidTransaction {
				priority: 1 << 20,
				requires: vec![],
				provides: vec![codec::Encode::encode(&(KEY_TYPE.0, block_num))],
				longevity: 3,
				propagate: false,
			})
		} else {
			InvalidTransaction::Call.into()
		}
	}
}
