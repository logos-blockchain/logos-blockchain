use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(transparent)]
pub struct InputDeser(pub String);

#[cfg(test)]
mod tests {
    use std::{ops::Deref as _, sync::LazyLock};

    use ark_bn254::Bn254;
    use serde_json::{Value, json};

    use crate::public_input::{Input, deserialize::InputDeser};

    static PI: LazyLock<Value> = LazyLock::new(|| {
        json!([
            "20355411533518962316551583414021767695266024416654937381718858980374314628607",
            "10",
            "125",
            "224544017050291497731587917515395456590972751373195855321023608221038158",
            "21888242870687514985294027879163787319377618759420784645983712289047175144239",
            "2560908565912827287507552665784789358607566611809861439260962961981744707910",
            "16032654658264631097783850361980666012858215179400660002665145514335493567351",
            "516548"
        ])
    });
    #[test]
    fn deserialize() {
        let pi: Vec<InputDeser> = serde_json::from_value(PI.deref().clone()).unwrap();
        drop(
            pi.into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<Input<Bn254>>, _>>(),
        );
    }
}
